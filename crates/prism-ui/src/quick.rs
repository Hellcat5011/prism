//! Quick answers for the launcher and Spotlight: a calculator (`2*(3+4)`, `sqrt 2`, `20% of
//! 340`), unit conversion (`5 km in mi`, `70 f to c`, `2 GiB in MB`), `>` to run a command, and
//! a web search for anything else. Enter copies a result, runs the command (Shift: in a
//! terminal) or opens the search.

/// One quick answer row.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum Answer {
    /// A calculation or conversion: what was asked and the result (copied on Enter).
    Value { question: String, value: String },
    /// `> command`.
    Command(String),
    /// Search the web for the query.
    Web(String),
}

impl Answer {
    pub fn title(&self) -> String {
        match self {
            Answer::Value { value, .. } => format!("= {value}"),
            Answer::Command(c) => c.clone(),
            Answer::Web(q) => format!("Search the web for \u{201c}{q}\u{201d}"),
        }
    }

    pub fn subtitle(&self) -> String {
        match self {
            Answer::Value { question, .. } => format!("{question} \u{b7} Enter copies"),
            Answer::Command(_) => "Run command \u{b7} Shift+Enter runs it in a terminal".into(),
            Answer::Web(_) => "Opens in the browser".into(),
        }
    }

    pub fn icon(&self) -> &'static str {
        match self {
            Answer::Value { .. } => "accessories-calculator",
            Answer::Command(_) => "utilities-terminal",
            Answer::Web(_) => "web-browser",
        }
    }

    /// Carry it out. `shift`: a command runs in a terminal.
    pub fn run(&self, shift: bool, web_url: &str) {
        match self {
            Answer::Value { value, .. } => {
                prism_common::wlclip::copy_text(value, true);
            }
            Answer::Command(c) => {
                let cmd = if shift {
                    let script = format!("{c}; echo; read -r -p 'Press Enter to close.' _");
                    prism_common::apps::terminal_command(&["sh".into(), "-c".into(), script])
                } else {
                    Some(prism_common::apps::app_command("sh", &["-c".into(), c.clone()]))
                };
                if let Some(mut cmd) = cmd {
                    if let Ok(child) = cmd.spawn() {
                        crate::reap(child);
                    }
                }
            }
            Answer::Web(q) => {
                let url = web_url.replace("{}", &url_encode(q));
                if let Ok(child) = prism_common::apps::app_command("xdg-open", &[url]).spawn() {
                    crate::reap(child);
                }
            }
        }
    }
}

/// Launcher entries for quick answers carry this id prefix and the answer as JSON in `exec`.
const APP_PREFIX: &str = "prism-quick:";

/// The launcher's results for `query`: quick answers (as entries every launcher design can
/// draw) above the matching apps; the web search only when no app matches.
pub fn launcher_results(apps: &[prism_common::apps::DesktopApp], query: &str) -> Vec<prism_common::apps::DesktopApp> {
    let mut found = prism_common::apps::search_apps(apps, query);
    let cfg = prism_common::Config::load();
    if !cfg.quick_answers {
        return found;
    }
    let web = found.is_empty() && !cfg.web_search_url.trim().is_empty();
    let mut out: Vec<_> = answers(query, web)
        .into_iter()
        .map(|a| prism_common::apps::DesktopApp {
            id: format!("{APP_PREFIX}{}", a.title()),
            name: a.title(),
            generic_name: Some(a.subtitle()),
            comment: Some(a.subtitle()),
            exec: serde_json::to_string(&a).unwrap_or_default(),
            icon: Some(a.icon().into()),
            categories: Vec::new(),
            keywords: Vec::new(),
            terminal: false,
            no_display: false,
            wm_class: None,
        })
        .collect();
    out.append(&mut found);
    out
}

/// Run a launcher entry if it is a quick answer; false for a real app.
pub fn launch_entry(app: &prism_common::apps::DesktopApp, shift: bool) -> bool {
    if !app.id.starts_with(APP_PREFIX) {
        return false;
    }
    if let Ok(a) = serde_json::from_str::<Answer>(&app.exec) {
        a.run(shift, &prism_common::Config::load().web_search_url);
    }
    true
}

/// The answers for `query`, best first. `web`: offer a web search.
pub fn answers(query: &str, web: bool) -> Vec<Answer> {
    let q = query.trim();
    if q.is_empty() {
        return Vec::new();
    }
    if let Some(cmd) = q.strip_prefix('>') {
        let cmd = cmd.trim();
        return if cmd.is_empty() { Vec::new() } else { vec![Answer::Command(cmd.to_string())] };
    }
    let mut out = Vec::new();
    if let Some(v) = convert(q).or_else(|| calculate(q)) {
        out.push(Answer::Value { question: q.to_string(), value: v });
    }
    if web {
        out.push(Answer::Web(q.to_string()));
    }
    out
}

fn url_encode(s: &str) -> String {
    s.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => (b as char).to_string(),
            b' ' => "+".into(),
            _ => format!("%{b:02X}"),
        })
        .collect()
}

/// A readable number: up to 10 significant digits, no trailing zeros, thousands kept plain.
pub fn format_number(v: f64) -> String {
    if !v.is_finite() {
        return "undefined".into();
    }
    if v != 0.0 && (v.abs() >= 1e15 || v.abs() < 1e-6) {
        return format!("{v:.6e}");
    }
    let digits = (10 - v.abs().log10().floor().max(0.0) as i32 - 1).clamp(0, 10) as usize;
    let s = format!("{v:.digits$}");
    let s = if s.contains('.') { s.trim_end_matches('0').trim_end_matches('.').to_string() } else { s };
    if s == "-0" { "0".into() } else { s }
}

// ── Calculator ──────────────────────────────────────────────────────────

/// The value of an arithmetic expression, or None when `q` is not one (a plain number or a
/// word is not a calculation).
pub fn calculate(q: &str) -> Option<String> {
    let q = q.trim().trim_end_matches('=').trim();
    let lower = q.to_lowercase();
    // "20% of 340"
    if let Some((a, b)) = lower.split_once("% of ") {
        let a = eval(a)?;
        let b = eval(b)?;
        return Some(format_number(a / 100.0 * b));
    }
    let tokens = tokenize(&lower)?;
    // Something to work out: an operator, a function or a constant beside a number.
    let worth = tokens.iter().any(|t| matches!(t, Tok::Op(_) | Tok::Ident(_) | Tok::Percent)) && tokens.len() > 1;
    if !worth {
        return None;
    }
    let mut p = Parser { toks: tokens, pos: 0, percent_rhs: false };
    let v = p.expr()?;
    (p.pos == p.toks.len()).then(|| format_number(v))
}

fn eval(s: &str) -> Option<f64> {
    let mut p = Parser { toks: tokenize(s.trim())?, pos: 0, percent_rhs: false };
    let v = p.expr()?;
    (p.pos == p.toks.len()).then_some(v)
}

#[derive(Clone, Debug, PartialEq)]
enum Tok {
    Num(f64),
    Op(char),
    LParen,
    RParen,
    Ident(String),
    Percent,
}

fn tokenize(s: &str) -> Option<Vec<Tok>> {
    let mut out = Vec::new();
    let mut chars = s.chars().peekable();
    while let Some(&c) = chars.peek() {
        if c.is_whitespace() {
            chars.next();
        } else if c.is_ascii_digit() || c == '.' {
            let mut n = String::new();
            while let Some(&d) = chars.peek() {
                if d.is_ascii_digit() || d == '.' || d == '_' {
                    if d != '_' {
                        n.push(d);
                    }
                    chars.next();
                } else {
                    break;
                }
            }
            out.push(Tok::Num(n.parse().ok()?));
        } else if c.is_alphabetic() {
            let mut w = String::new();
            while let Some(&d) = chars.peek() {
                if d.is_alphanumeric() {
                    w.push(d);
                    chars.next();
                } else {
                    break;
                }
            }
            // "3 x 4": x between numbers multiplies.
            if w == "x" && matches!(out.last(), Some(Tok::Num(_)) | Some(Tok::RParen)) {
                out.push(Tok::Op('*'));
            } else {
                out.push(Tok::Ident(w));
            }
        } else {
            chars.next();
            out.push(match c {
                '+' | '-' | '*' | '/' | '^' => Tok::Op(c),
                '\u{d7}' => Tok::Op('*'),
                '\u{f7}' => Tok::Op('/'),
                '(' => Tok::LParen,
                ')' => Tok::RParen,
                '%' => Tok::Percent,
                _ => return None,
            });
        }
    }
    Some(out)
}

struct Parser {
    toks: Vec<Tok>,
    pos: usize,
    /// The last operand parsed ended in % (for "200 + 10%").
    percent_rhs: bool,
}

impl Parser {
    fn peek(&self) -> Option<&Tok> {
        self.toks.get(self.pos)
    }

    fn expr(&mut self) -> Option<f64> {
        let mut v = self.term()?;
        while let Some(Tok::Op(c @ ('+' | '-'))) = self.peek().cloned() {
            self.pos += 1;
            self.percent_rhs = false;
            let rhs = self.term()?;
            // "200 + 10%" adds ten percent of 200.
            let rhs = if self.percent_rhs && self.toks.get(self.pos - 1) == Some(&Tok::Percent) { v * rhs } else { rhs };
            self.percent_rhs = false;
            v = if c == '+' { v + rhs } else { v - rhs };
        }
        Some(v)
    }

    fn term(&mut self) -> Option<f64> {
        let mut v = self.power()?;
        loop {
            match self.peek().cloned() {
                Some(Tok::Op(c @ ('*' | '/'))) => {
                    self.pos += 1;
                    let rhs = self.power()?;
                    v = if c == '*' { v * rhs } else { v / rhs };
                }
                // Implicit multiplication: "2pi", "3(4+1)".
                Some(Tok::LParen) | Some(Tok::Ident(_)) => v *= self.power()?,
                _ => return Some(v),
            }
        }
    }

    fn power(&mut self) -> Option<f64> {
        let base = self.unary()?;
        if let Some(Tok::Op('^')) = self.peek() {
            self.pos += 1;
            let exp = self.power()?;
            return Some(base.powf(exp));
        }
        Some(base)
    }

    fn unary(&mut self) -> Option<f64> {
        match self.peek() {
            Some(Tok::Op('-')) => {
                self.pos += 1;
                Some(-self.unary()?)
            }
            Some(Tok::Op('+')) => {
                self.pos += 1;
                self.unary()
            }
            _ => self.postfix(),
        }
    }

    fn postfix(&mut self) -> Option<f64> {
        let v = self.atom()?;
        if let Some(Tok::Percent) = self.peek() {
            self.pos += 1;
            self.percent_rhs = true;
            return Some(v / 100.0);
        }
        Some(v)
    }

    fn atom(&mut self) -> Option<f64> {
        match self.peek().cloned()? {
            Tok::Num(n) => {
                self.pos += 1;
                Some(n)
            }
            Tok::LParen => {
                self.pos += 1;
                let v = self.expr()?;
                if self.peek() != Some(&Tok::RParen) {
                    return None;
                }
                self.pos += 1;
                Some(v)
            }
            Tok::Ident(name) => {
                self.pos += 1;
                match name.as_str() {
                    "pi" => Some(std::f64::consts::PI),
                    "e" => Some(std::f64::consts::E),
                    "tau" => Some(std::f64::consts::TAU),
                    f => {
                        let x = self.unary()?;
                        Some(match f {
                            "sqrt" => x.sqrt(),
                            "cbrt" => x.cbrt(),
                            "sin" => x.to_radians().sin(),
                            "cos" => x.to_radians().cos(),
                            "tan" => x.to_radians().tan(),
                            "ln" => x.ln(),
                            "log" => x.log10(),
                            "abs" => x.abs(),
                            "round" => x.round(),
                            "floor" => x.floor(),
                            "ceil" => x.ceil(),
                            "exp" => x.exp(),
                            _ => return None,
                        })
                    }
                }
            }
            _ => None,
        }
    }
}

// ── Units ───────────────────────────────────────────────────────────────

/// (names, category, factor to the category's base unit). Temperatures are handled apart.
const UNITS: &[(&[&str], &str, f64)] = &[
    (&["mm", "millimeter", "millimeters", "millimetre", "millimetres"], "length", 0.001),
    (&["cm", "centimeter", "centimeters", "centimetre", "centimetres"], "length", 0.01),
    (&["m", "meter", "meters", "metre", "metres"], "length", 1.0),
    (&["km", "kilometer", "kilometers", "kilometre", "kilometres"], "length", 1000.0),
    (&["in", "inch", "inches", "\""], "length", 0.0254),
    (&["ft", "foot", "feet", "'"], "length", 0.3048),
    (&["yd", "yard", "yards"], "length", 0.9144),
    (&["mi", "mile", "miles"], "length", 1609.344),
    (&["nmi", "nautical"], "length", 1852.0),
    (&["mg", "milligram", "milligrams"], "mass", 0.001),
    (&["g", "gram", "grams"], "mass", 1.0),
    (&["kg", "kilogram", "kilograms", "kilo", "kilos"], "mass", 1000.0),
    (&["t", "tonne", "tonnes", "ton", "tons"], "mass", 1_000_000.0),
    (&["oz", "ounce", "ounces"], "mass", 28.349523125),
    (&["lb", "lbs", "pound", "pounds"], "mass", 453.59237),
    (&["st", "stone", "stones"], "mass", 6350.29318),
    (&["ml", "milliliter", "milliliters", "millilitre", "millilitres"], "volume", 0.001),
    (&["cl", "centiliter", "centiliters"], "volume", 0.01),
    (&["l", "liter", "liters", "litre", "litres"], "volume", 1.0),
    (&["gal", "gallon", "gallons"], "volume", 3.785411784),
    (&["qt", "quart", "quarts"], "volume", 0.946352946),
    (&["pt", "pint", "pints"], "volume", 0.473176473),
    (&["cup", "cups"], "volume", 0.2365882365),
    (&["floz", "fl oz"], "volume", 0.0295735295625),
    (&["tbsp", "tablespoon", "tablespoons"], "volume", 0.01478676478125),
    (&["tsp", "teaspoon", "teaspoons"], "volume", 0.00492892159375),
    (&["b", "byte", "bytes"], "data", 1.0),
    (&["kb", "kilobyte", "kilobytes"], "data", 1e3),
    (&["mb", "megabyte", "megabytes"], "data", 1e6),
    (&["gb", "gigabyte", "gigabytes"], "data", 1e9),
    (&["tb", "terabyte", "terabytes"], "data", 1e12),
    (&["kib", "kibibyte", "kibibytes"], "data", 1024.0),
    (&["mib", "mebibyte", "mebibytes"], "data", 1048576.0),
    (&["gib", "gibibyte", "gibibytes"], "data", 1073741824.0),
    (&["tib", "tebibyte", "tebibytes"], "data", 1099511627776.0),
    (&["ms", "millisecond", "milliseconds"], "time", 0.001),
    (&["s", "sec", "secs", "second", "seconds"], "time", 1.0),
    (&["min", "mins", "minute", "minutes"], "time", 60.0),
    (&["h", "hr", "hrs", "hour", "hours"], "time", 3600.0),
    (&["d", "day", "days"], "time", 86400.0),
    (&["wk", "week", "weeks"], "time", 604800.0),
    (&["kmh", "km/h", "kph"], "speed", 1.0 / 3.6),
    (&["mph"], "speed", 0.44704),
    (&["m/s", "mps"], "speed", 1.0),
    (&["kn", "kt", "knot", "knots"], "speed", 0.514444),
];

fn unit(name: &str) -> Option<(&'static str, f64, &'static str)> {
    let n = name.trim().trim_end_matches('.');
    UNITS.iter().find(|(names, _, _)| names.iter().any(|x| x.eq_ignore_ascii_case(n))).map(|(names, cat, f)| (*cat, *f, names[0]))
}

fn temp_unit(name: &str) -> Option<char> {
    match name.trim().to_lowercase().trim_start_matches('\u{b0}') {
        "c" | "celsius" | "degc" => Some('C'),
        "f" | "fahrenheit" | "degf" => Some('F'),
        "k" | "kelvin" => Some('K'),
        _ => None,
    }
}

/// "<number> <unit> in|to <unit>".
pub fn convert(q: &str) -> Option<String> {
    let lower = q.trim().to_lowercase();
    let (lhs, to) = lower.split_once(" in ").or_else(|| lower.split_once(" to ")).or_else(|| lower.split_once(" as "))?;
    let lhs = lhs.trim();
    let split = lhs.find(|c: char| !(c.is_ascii_digit() || c == '.' || c == '-' || c == ',' || c == ' ' || c == '_'))?;
    let (num, from) = lhs.split_at(split);
    let value: f64 = num.replace([',', '_', ' '], "").parse().ok()?;
    let (from, to) = (from.trim(), to.trim());
    if let (Some(a), Some(b)) = (temp_unit(from), temp_unit(to)) {
        let k = match a {
            'C' => value + 273.15,
            'F' => (value - 32.0) * 5.0 / 9.0 + 273.15,
            _ => value,
        };
        let out = match b {
            'C' => k - 273.15,
            'F' => (k - 273.15) * 9.0 / 5.0 + 32.0,
            _ => k,
        };
        let sym = |c: char| if c == 'K' { "K".to_string() } else { format!("\u{b0}{c}") };
        return Some(format!("{} {}", format_number((out * 100.0).round() / 100.0), sym(b)));
    }
    let (ca, fa, _) = unit(from)?;
    let (cb, fb, nb) = unit(to)?;
    if ca != cb {
        return None;
    }
    Some(format!("{} {nb}", format_number(value * fa / fb)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn arithmetic() {
        assert_eq!(calculate("2*(3+4)").as_deref(), Some("14"));
        assert_eq!(calculate("2^10").as_deref(), Some("1024"));
        assert_eq!(calculate("10 / 4").as_deref(), Some("2.5"));
        assert_eq!(calculate("-3 + 5").as_deref(), Some("2"));
        assert_eq!(calculate("sqrt 16").as_deref(), Some("4"));
        assert_eq!(calculate("2pi").as_deref(), Some("6.283185307"));
        assert_eq!(calculate("20% of 340").as_deref(), Some("68"));
        assert_eq!(calculate("200 + 10%").as_deref(), Some("220"));
        assert_eq!(calculate("1/3").as_deref(), Some("0.333333333"));
    }

    #[test]
    fn not_a_calculation() {
        assert_eq!(calculate("firefox"), None);
        assert_eq!(calculate("2024"), None);
        assert_eq!(calculate("2 +"), None);
        assert_eq!(calculate("hello world"), None);
    }

    #[test]
    fn units() {
        assert_eq!(convert("5 km in mi").as_deref(), Some("3.106855961 mi"));
        assert_eq!(convert("100 f to c").as_deref(), Some("37.78 \u{b0}C"));
        assert_eq!(convert("2 GiB in MB").as_deref(), Some("2147.483648 mb"));
        assert_eq!(convert("1 lb in g").as_deref(), Some("453.59237 g"));
        assert_eq!(convert("5 km in kg"), None);
        assert_eq!(convert("text in other"), None);
    }

    #[test]
    fn answers_order() {
        assert_eq!(answers("> ls -la", true), vec![Answer::Command("ls -la".into())]);
        let a = answers("2+2", true);
        assert_eq!(a[0], Answer::Value { question: "2+2".into(), value: "4".into() });
        assert_eq!(a[1], Answer::Web("2+2".into()));
        assert_eq!(answers("firefox", false), Vec::new());
    }
}
