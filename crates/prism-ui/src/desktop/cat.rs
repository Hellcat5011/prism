// ─────────────────────────────────────────────────────────────────────────
// desktop/cat.rs — the Cat widget
//
// A black cat that sleeps on the desktop. Hovering wakes it: it stretches its way
// up and keeps itself busy until the pointer leaves, then settles down again. It
// is drawn from a few joints in code, so it scales to any size, and every frame
// is a pure function of the time since the current action began.
// ─────────────────────────────────────────────────────────────────────────

use std::f64::consts::{FRAC_PI_2, PI, TAU};
use std::time::Instant;

use crate::cairo_backend::*;

use super::widgets::{Look, Rgba};

/// The drawing is designed for this box; the widget scales it to fit. Nothing leaves it.
const DESIGN_W: f64 = prism_common::config::CAT_SIZE.0;
const DESIGN_H: f64 = prism_common::config::CAT_SIZE.1;
/// Space under the feet, in design units: the cat sits in the middle of the widget.
const GROUND: f64 = 50.0;
/// Seconds an action takes to spring out of sitting, and back. Cats do not ease into things.
const BLEND: f64 = 0.24;
/// The walk starts and ends more calmly.
const WALK_BLEND: f64 = 0.55;
/// Seconds to cross from whatever the cat was doing into the next thing.
const CROSSFADE: f64 = 0.3;
const WAKING: f64 = 3.4;
const SETTLING: f64 = 3.6;
/// Seconds a turn takes at each end of a walk.
const TURN: f64 = 0.45;
/// Height of the little hop the cat pivots on, so that its feet never shuffle on the spot.
const TURN_HOP: f64 = 8.0;
const CAIRO_OPERATOR_ATOP: i32 = 5;

/// What the cat does while it is awake.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Act {
    Sit,
    Look,
    Yawn,
    Groom,
    Stretch,
    Walk,
    Pounce,
    Yarn,
    Butterfly,
    Knead,
    Scratch,
    Hearts,
}

impl Act {
    const ALL: [Act; 12] = [Act::Sit, Act::Look, Act::Yawn, Act::Groom, Act::Stretch, Act::Walk, Act::Pounce, Act::Yarn, Act::Butterfly, Act::Knead, Act::Scratch, Act::Hearts];

    fn seconds(self) -> f64 {
        match self {
            Act::Sit => 2.6,
            Act::Look => 3.4,
            Act::Yawn => 2.4,
            Act::Groom => 4.0,
            Act::Stretch => 3.0,
            Act::Walk => 10.7,
            Act::Pounce => 3.0,
            Act::Yarn => 6.5,
            Act::Butterfly => 5.4,
            Act::Knead => 4.0,
            Act::Scratch => 3.4,
            Act::Hearts => 3.6,
        }
    }
}

type P = (f64, f64);

fn lerp(a: f64, b: f64, t: f64) -> f64 {
    a + (b - a) * t
}

fn lerp_p(a: P, b: P, t: f64) -> P {
    (lerp(a.0, b.0, t), lerp(a.1, b.1, t))
}

fn smooth(t: f64) -> f64 {
    let t = t.clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// Rises over `rise` seconds from `from`, holds, and falls back by `to`.
fn bump(t: f64, from: f64, to: f64, rise: f64) -> f64 {
    smooth((t - from) / rise).min(smooth((to - t) / rise))
}

/// Starts slowly and arrives fast: how a cat's head or ear snaps to something.
fn accel(t: f64) -> f64 {
    t.clamp(0.0, 1.0).powi(3)
}

/// Like `bump`, but darting out on the accelerating curve.
fn pop(t: f64, from: f64, to: f64, rise: f64) -> f64 {
    accel((t - from) / rise).min(smooth((to - t) / (rise * 1.3)))
}

/// A repeatable random value in -1..1 for step `n`.
fn noise(n: f64, salt: f64) -> f64 {
    ((n * 12.9898 + salt * 78.233).sin() * 43758.5453).rem_euclid(1.0) * 2.0 - 1.0
}

/// Holds still, then darts to a new value every `every` seconds.
fn dart(t: f64, every: f64, salt: f64) -> f64 {
    let n = (t / every).floor();
    let held = t - n * every;
    lerp(noise(n - 1.0, salt), noise(n, salt), accel(held / 0.13))
}

/// A quick flick now and then: up on the accelerating curve, straight back.
fn flick(t: f64, every: f64, salt: f64) -> f64 {
    let n = (t / every).floor();
    let at = (0.2 + 0.3 * (noise(n, salt) + 1.0)) * every;
    let held = t - n * every - at;
    accel(held / 0.07).min(1.0 - smooth((held - 0.07) / 0.1))
}

/// Where the joints are, around the spot on the ground under the cat.
///
/// `tail` is the direction the tail leaves in. Every pose keeps it between pointing left (PI)
/// and straight up (3/2 PI), so blending two poses always swings it over the cat's back.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Body {
    hip: P,
    hip_r: f64,
    shoulder: P,
    shoulder_r: f64,
    head: P,
    fore: P,
    fore_far: P,
    hind: P,
    hind_far: P,
    tail: f64,
    curl: f64,
}

const SIT: Body = Body {
    hip: (-24.0, -34.0),
    hip_r: 33.0,
    shoulder: (8.0, -76.0),
    shoulder_r: 20.0,
    head: (20.0, -118.0),
    fore: (20.0, -7.0),
    fore_far: (6.0, -7.0),
    hind: (2.0, -7.0),
    hind_far: (-14.0, -7.0),
    tail: 3.3,
    curl: 0.8,
};

const STAND: Body = Body {
    hip: (-44.0, -66.0),
    hip_r: 24.0,
    shoulder: (32.0, -70.0),
    shoulder_r: 23.0,
    head: (62.0, -108.0),
    fore: (38.0, -7.0),
    fore_far: (26.0, -7.0),
    hind: (-46.0, -7.0),
    hind_far: (-34.0, -7.0),
    tail: 4.2,
    curl: 1.0,
};

/// Facing the viewer, in the middle of a turn: the same seen from either side.
const FRONT: Body = Body {
    hip: (0.0, -50.0),
    hip_r: 27.0,
    shoulder: (0.0, -76.0),
    shoulder_r: 23.0,
    head: (0.0, -116.0),
    fore: (10.0, -7.0),
    fore_far: (-10.0, -7.0),
    // The legs keep their sides on the way here, so that they never cross.
    hind: (-24.0, -7.0),
    hind_far: (24.0, -7.0),
    tail: 3.0 * FRAC_PI_2,
    curl: 0.0,
};

/// Up on the hind legs, reaching for something.
const REAR: Body = Body {
    hip: (-8.0, -46.0),
    hip_r: 29.0,
    shoulder: (18.0, -94.0),
    shoulder_r: 20.0,
    head: (32.0, -130.0),
    fore: (46.0, -80.0),
    fore_far: (34.0, -66.0),
    hind: (4.0, -7.0),
    hind_far: (-14.0, -7.0),
    tail: 3.7,
    curl: 0.7,
};

const STRETCH: Body = Body {
    hip: (-46.0, -74.0),
    hip_r: 25.0,
    shoulder: (30.0, -32.0),
    shoulder_r: 20.0,
    head: (66.0, -46.0),
    fore: (84.0, -7.0),
    fore_far: (70.0, -7.0),
    hind: (-50.0, -7.0),
    hind_far: (-38.0, -7.0),
    tail: 4.4,
    curl: 0.6,
};

const CROUCH: Body = Body {
    hip: (-42.0, -42.0),
    hip_r: 27.0,
    shoulder: (24.0, -30.0),
    shoulder_r: 21.0,
    head: (56.0, -54.0),
    fore: (50.0, -7.0),
    fore_far: (38.0, -7.0),
    hind: (-30.0, -7.0),
    hind_far: (-18.0, -7.0),
    tail: 3.3,
    curl: 0.5,
};

const LOAF: Body = Body {
    hip: (-34.0, -30.0),
    hip_r: 30.0,
    shoulder: (16.0, -28.0),
    shoulder_r: 25.0,
    head: (50.0, -36.0),
    fore: (42.0, -8.0),
    fore_far: (30.0, -8.0),
    hind: (-12.0, -8.0),
    hind_far: (-24.0, -8.0),
    tail: PI,
    curl: 0.45,
};

fn blend(a: &Body, b: &Body, t: f64) -> Body {
    Body {
        hip: lerp_p(a.hip, b.hip, t),
        hip_r: lerp(a.hip_r, b.hip_r, t),
        shoulder: lerp_p(a.shoulder, b.shoulder, t),
        shoulder_r: lerp(a.shoulder_r, b.shoulder_r, t),
        head: lerp_p(a.head, b.head, t),
        fore: lerp_p(a.fore, b.fore, t),
        fore_far: lerp_p(a.fore_far, b.fore_far, t),
        hind: lerp_p(a.hind, b.hind, t),
        hind_far: lerp_p(a.hind_far, b.hind_far, t),
        tail: lerp(a.tail, b.tail, t),
        curl: lerp(a.curl, b.curl, t),
    }
}

/// Everything one frame shows.
#[derive(Clone, Copy, Debug)]
struct Pose {
    body: Body,
    x: f64,
    lift: f64,
    /// 1 facing right, -1 facing left. It only changes while the cat faces the viewer.
    facing: f64,
    eyes: f64,
    look: f64,
    mouth: f64,
    tongue: f64,
    tilt: f64,
    ear: f64,
    /// Seconds into the nap, for the rising letters.
    zzz: Option<f64>,
    /// Ball of yarn: x on the ground, how far it has rolled, and size 0..1.
    yarn: Option<(f64, f64, f64)>,
    /// Butterfly: position, wing beat 0..1 and visibility.
    fly: Option<(f64, f64, f64, f64)>,
    /// Seconds into the hearts.
    hearts: Option<f64>,
}

fn sitting(t: f64) -> Pose {
    let mut body = SIT;
    body.tail += (t * 1.4).sin() * 0.1;
    body.curl += (t * 1.1).sin() * 0.15;
    Pose { body, x: 0.0, lift: 0.0, facing: 1.0, eyes: blink(t), look: 0.0, mouth: 0.0, tongue: 0.0, tilt: 0.0, ear: 0.0, zzz: None, yarn: None, fly: None, hearts: None }
}

/// Eyes open, closing briefly every few seconds.
fn blink(t: f64) -> f64 {
    let phase = (t + 0.7).rem_euclid(3.6);
    1.0 - bump(phase, 0.0, 0.2, 0.08)
}

/// Out to one side, a turn, across to the other, a turn, and back to the middle.
/// Returns the position, the way the cat faces, how far into a turn it is and whether it steps.
fn walk_path(t: f64, roam: f64) -> (f64, f64, f64, f64) {
    let leg = (Act::Walk.seconds() - 2.0 * WALK_BLEND - 2.0 * TURN) / 4.0;
    let s = (t - WALK_BLEND).max(0.0);
    let turning = |from: f64| bump(s - from, 0.0, TURN, 0.18);
    let stepping = |from: f64, len: f64| bump(s - from, 0.0, len, 0.25);
    let first = leg + TURN;
    let second = first + 2.0 * leg + TURN;
    if s < leg {
        (roam * smooth(s / leg), 1.0, 0.0, stepping(0.0, leg))
    } else if s < first {
        (roam, if s < leg + TURN / 2.0 { 1.0 } else { -1.0 }, turning(leg), 0.0)
    } else if s < first + 2.0 * leg {
        (roam * (1.0 - 2.0 * smooth((s - first) / (2.0 * leg))), -1.0, 0.0, stepping(first, 2.0 * leg))
    } else if s < second {
        (-roam, if s < first + 2.0 * leg + TURN / 2.0 { -1.0 } else { 1.0 }, turning(first + 2.0 * leg), 0.0)
    } else {
        (-roam * (1.0 - smooth((s - second) / leg)), 1.0, 0.0, stepping(second, leg))
    }
}

/// The butterfly: where it is at `t` and how visible. It flutters in reach, dodges the first
/// swat and escapes the second.
fn butterfly_at(t: f64) -> (f64, f64, f64) {
    let (x, y) = (100.0 + 12.0 * (t * 2.6).sin(), -128.0 + 11.0 * (t * 4.1).sin());
    let gone = (t - BUTTERFLY_HIT).max(0.0);
    let show = smooth(t / 0.3) * (1.0 - smooth(gone / 0.9));
    ((x + 60.0 * gone * gone).min(142.0), (y - 110.0 * gone).max(-216.0), show)
}

/// The moments the paw reaches the butterfly.
const BUTTERFLY_SWAT: f64 = 1.72;
const BUTTERFLY_HIT: f64 = 3.3;

fn pose(act: Act, t: f64, roam: f64) -> Pose {
    let total = act.seconds();
    let spring = if act == Act::Walk { WALK_BLEND } else { BLEND };
    let env = smooth(t / spring).min(smooth((total - t) / spring));
    let mut p = sitting(t);
    match act {
        Act::Sit => {
            p.look = env * 0.55 * dart(t, 0.8, 1.0);
            p.tilt = env * 0.1 * dart(t, 1.1, 2.0);
            p.ear = env * flick(t, 1.1, 3.0);
        }
        Act::Look => {
            p.look = env * dart(t, 0.55, 4.0);
            p.tilt = env * 0.2 * dart(t, 0.75, 5.0);
            p.ear = env * flick(t, 0.8, 6.0);
            p.body.head.1 -= env * 5.0 * dart(t, 0.9, 7.0).max(0.0);
        }
        Act::Yawn => {
            let wide = pop(t, 0.15, 1.65, 0.24);
            p.mouth = wide;
            p.eyes = p.eyes.min(1.0 - wide);
            p.tilt = -0.3 * wide;
            p.body.head.1 -= 7.0 * wide;
            // A quick shake of the head afterwards.
            let shake = bump(t, 1.7, 2.2, 0.06);
            p.tilt += 0.17 * shake * (t * 42.0).sin();
            p.ear = shake;
        }
        Act::Groom => {
            let up = pop(t, 0.1, total - 0.2, 0.2);
            let lick = (t * 13.0).sin();
            p.body.head = lerp_p(p.body.head, (p.body.head.0 + 4.0, p.body.head.1 + 8.0), up);
            let paw = (p.body.head.0 + 10.0, p.body.head.1 + 27.0 + 4.0 * lick);
            p.body.fore = lerp_p(p.body.fore, paw, up);
            p.tilt = 0.2 * up + 0.08 * up * lick;
            p.eyes = p.eyes.min(1.0 - 0.9 * up);
            p.tongue = up * (0.5 + 0.5 * lick);
            p.ear = up * flick(t, 0.9, 8.0);
        }
        Act::Stretch => {
            let reach = pop(t, 0.1, total - 0.2, 0.4);
            p.body = blend(&p.body, &STRETCH, reach);
            p.eyes = p.eyes.min(1.0 - reach);
            p.mouth = 0.35 * pop(t, 0.6, 2.2, 0.25);
            p.tilt = -0.16 * reach;
            p.body.hip.0 += reach * 2.0 * (t * 26.0).sin() * bump(t, 2.2, 2.8, 0.1);
        }
        Act::Walk => {
            let (x, facing, turn, steps) = walk_path(t, roam);
            p.x = x;
            p.facing = facing;
            p.body = blend(&p.body, &STAND, env);
            let step = t * 8.5;
            let place = |at: &mut P, phase: f64| {
                at.0 += steps * (step + phase).sin() * 17.0;
                at.1 -= steps * (step + phase).cos().max(0.0) * 11.0;
            };
            place(&mut p.body.fore, 0.0);
            place(&mut p.body.fore_far, PI);
            place(&mut p.body.hind, FRAC_PI_2);
            place(&mut p.body.hind_far, 3.0 * FRAC_PI_2);
            p.body.hip.1 += steps * 2.0 * (step * 2.0).sin();
            p.body.shoulder.1 += steps * 2.0 * (step * 2.0 + 1.0).sin();
            // Each stride ends in a sharp little nod.
            let stride = (step * 2.0).rem_euclid(TAU) / TAU;
            p.body.head.1 += steps * 5.0 * accel(stride / 0.8).min(1.0 - smooth((stride - 0.8) / 0.2));
            p.body.tail += env * 0.12 * (t * 3.0).sin() * (1.0 - turn);
            p.look = env * (1.0 - turn) * 0.7 * dart(t, 0.7, 9.0);
            p.tilt = env * (1.0 - turn) * 0.14 * dart(t, 0.95, 10.0);
            p.ear = env * flick(t, 0.75, 11.0);
            p.body = blend(&p.body, &FRONT, turn);
            p.lift = TURN_HOP * turn;
        }
        Act::Pounce => {
            let down = bump(t, 0.1, 2.1, 0.14);
            p.body = blend(&p.body, &CROUCH, down);
            let aim = bump(t, 0.35, 1.55, 0.1);
            p.body.hip.0 += aim * 4.0 * (t * 30.0).sin();
            p.body.tail += aim * 0.2 * (t * 17.0).sin();
            let hop = ((t - 1.55) / 0.5).clamp(0.0, 1.0);
            if hop > 0.0 && hop < 1.0 {
                p.lift = 58.0 * 4.0 * hop * (1.0 - hop);
                p.body = blend(&p.body, &STRETCH, 0.45 * (PI * hop).sin());
            }
            p.ear = aim;
            p.look = env * 0.5 * dart(t, 0.5, 12.0) * (1.0 - aim);
        }
        Act::Yarn => {
            // The paw bats the ball away, and it rolls back for the next go.
            let cycle = (t * 0.95).rem_euclid(1.0);
            let x = 84.0 + 44.0 * (PI * cycle).sin().powi(2);
            let bat = pop(cycle, 0.0, 0.22, 0.09) * env;
            p.body = blend(&p.body, &CROUCH, 0.5 * env);
            p.body.fore = lerp_p(p.body.fore, (x - 24.0, -10.0 - 12.0 * bat), 0.35 + 0.65 * bat * env);
            p.lift = 9.0 * bat;
            p.look = env * (0.65 + 0.3 * dart(t, 0.45, 13.0));
            p.tilt = env * 0.14;
            p.ear = env * flick(t, 0.7, 14.0);
            p.body.tail += env * 0.18 * (t * 9.0).sin();
            p.yarn = Some((x, x / 15.0, env));
        }
        Act::Butterfly => {
            let (fx, fy, show) = butterfly_at(t);
            p.fly = Some((fx, fy, (t * 34.0).sin().abs(), show));
            // Up on the hind legs for a swat, then down, ready, and a leap for it.
            let stand = pop(t, 1.4, 2.1, 0.26);
            let ready = bump(t, 2.45, 3.0, 0.1);
            let jump = ((t - 2.98) / 0.66).clamp(0.0, 1.0);
            p.lift = 32.0 * 4.0 * jump * (1.0 - jump);
            let leap = if jump > 0.0 && jump < 1.0 { smooth(jump / 0.25).min(smooth((1.0 - jump) / 0.25)) } else { 0.0 };
            p.body = blend(&p.body, &CROUCH, ready);
            p.body = blend(&p.body, &REAR, stand.max(leap));
            p.body.hip.0 += ready * 3.5 * (t * 30.0).sin();
            let swat = pop(t, BUTTERFLY_SWAT - 0.16, BUTTERFLY_SWAT + 0.2, 0.16).max(pop(t, BUTTERFLY_HIT - 0.16, BUTTERFLY_HIT + 0.22, 0.16));
            p.body.fore = lerp_p(p.body.fore, (fx, fy + p.lift), swat);
            p.look = env * ((fx - p.body.head.0) / 38.0).clamp(-1.0, 1.0);
            p.tilt = env * -0.24;
            p.ear = stand.max(ready);
        }
        Act::Knead => {
            let on = pop(t, 0.1, total - 0.15, 0.2);
            let press = (t * 9.0).sin();
            p.body.fore.1 -= on * press.max(0.0) * 13.0;
            p.body.fore_far.1 -= on * (-press).max(0.0) * 13.0;
            p.body.shoulder.1 += on * 2.5 * press.abs();
            p.body.head.1 += on * (3.0 + 3.0 * press.abs());
            p.eyes = p.eyes.min(1.0 - 0.72 * on);
            p.tilt = on * 0.12 * dart(t, 0.6, 15.0);
            p.ear = on * flick(t, 0.8, 16.0);
        }
        Act::Scratch => {
            let on = pop(t, 0.1, total - 0.2, 0.2);
            let shake = (t * 38.0).sin();
            let foot = (p.body.head.0 - 12.0, p.body.head.1 + 30.0 + 5.0 * shake);
            p.body.hind = lerp_p(p.body.hind, foot, on);
            p.tilt = -0.3 * on + 0.05 * on * shake;
            p.eyes = p.eyes.min(1.0 - on);
            p.ear = on * (0.5 + 0.5 * shake);
        }
        Act::Hearts => {
            // Happy little hops, eyes squeezed shut on each landing.
            let hop = (t * 6.6).sin().abs();
            p.lift = env * 11.0 * hop;
            p.eyes = p.eyes.min(1.0 - env * accel((1.0 - hop) / 0.5));
            p.tilt = env * 0.2 * dart(t, 0.45, 17.0);
            p.ear = env * flick(t, 0.6, 18.0);
            p.body.tail += env * 0.2 * (t * 13.0).sin();
            if t > 0.15 && t < total - 0.1 {
                p.hearts = Some(t - 0.15);
            }
        }
    }
    p
}

/// Curled up asleep, breathing slowly.
fn asleep(t: f64) -> Pose {
    let breath = (t * 1.5).sin();
    let mut p = sitting(0.0);
    p.body = LOAF;
    p.body.shoulder_r += 1.2 * breath;
    p.body.hip_r += 1.0 * breath;
    p.body.head.1 += 1.2 * breath;
    p.body.tail += 0.02 * (t * 0.7).sin();
    p.eyes = 0.0;
    p.tilt = 0.14;
    p.zzz = Some(t);
    p
}

/// From asleep to sitting: an ear first, heavy eyelids, a yawn, then up.
fn waking(t: f64) -> Pose {
    let mut p = asleep(t);
    p.zzz = None;
    let up = smooth((t - 1.0) / 1.9);
    p.body = blend(&p.body, &sitting(t).body, up);
    p.ear = bump(t, 0.05, 0.55, 0.12);
    // The eyes open, sink shut once more, and stay open.
    let first = 0.65 * bump(t, 0.45, 1.25, 0.28);
    p.eyes = first.max(smooth((t - 1.5) / 0.7));
    let yawn = bump(t, 1.5, 2.9, 0.45);
    p.mouth = 0.8 * yawn;
    p.eyes = p.eyes.min(1.0 - yawn);
    p.tilt = lerp(0.14, 0.0, up) - 0.2 * yawn;
    p
}

/// From sitting to asleep: a yawn, the eyes growing heavy, then down.
fn settling(t: f64) -> Pose {
    let mut p = sitting(t);
    let yawn = bump(t, 0.1, 1.4, 0.4);
    p.mouth = 0.7 * yawn;
    p.tilt = -0.18 * yawn;
    let down = smooth((t - 1.2) / 2.1);
    let rest = asleep(t);
    p.body = blend(&p.body, &rest.body, down);
    p.tilt = lerp(p.tilt, rest.tilt, down);
    // Two slow, sinking blinks before they stay shut.
    let heavy = (1.0 - smooth((t - 1.1) / 1.6)) * (0.8 + 0.2 * (t * 5.0).cos());
    p.eyes = p.eyes.min(1.0 - yawn).min(heavy.max(0.0));
    p
}

fn crossfade(a: &Pose, b: &Pose, t: f64) -> Pose {
    Pose {
        body: blend(&a.body, &b.body, t),
        x: lerp(a.x, b.x, t),
        lift: lerp(a.lift, b.lift, t),
        eyes: lerp(a.eyes, b.eyes, t),
        look: lerp(a.look, b.look, t),
        mouth: lerp(a.mouth, b.mouth, t),
        tongue: lerp(a.tongue, b.tongue, t),
        tilt: lerp(a.tilt, b.tilt, t),
        ear: lerp(a.ear, b.ear, t),
        ..*b
    }
}

// ── What the cat is doing ─────────────────────────────────────────────────

#[derive(Clone, Copy, Debug, PartialEq)]
enum Phase {
    Asleep,
    Waking,
    Awake(Act),
    Settling,
}

pub struct CatState {
    phase: Phase,
    started: Instant,
    hovered: bool,
    /// The actions still to come this round: every one plays before any plays again.
    bag: Vec<Act>,
    /// The frame the current phase blends in from, and when that began.
    from: Option<(Pose, Instant)>,
    shown: Pose,
    seed: u64,
}

impl CatState {
    pub fn new() -> CatState {
        let seed = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(1, |d| d.as_nanos() as u64) | 1;
        CatState { phase: Phase::Asleep, started: Instant::now(), hovered: false, bag: Vec::new(), from: None, shown: asleep(0.0), seed }
    }

    /// The pointer came onto the cat or left it.
    pub fn hover(&mut self, on: bool) {
        self.hovered = on;
    }

    /// Asleep with nobody around: the slow breathing needs few frames.
    pub fn resting(&self) -> bool {
        self.phase == Phase::Asleep && !self.hovered
    }

    fn random(&mut self) -> u64 {
        self.seed ^= self.seed << 13;
        self.seed ^= self.seed >> 7;
        self.seed ^= self.seed << 17;
        self.seed
    }

    /// The next action of the round, never the one that just ended.
    fn next_act(&mut self, last: Act) -> Act {
        if self.bag.is_empty() {
            self.bag = Act::ALL.to_vec();
            for i in (1..self.bag.len()).rev() {
                let j = (self.random() % (i as u64 + 1)) as usize;
                self.bag.swap(i, j);
            }
        }
        let n = self.bag.len();
        if n > 1 && self.bag[n - 1] == last {
            self.bag.swap(0, n - 1);
        }
        self.bag.pop().unwrap_or(Act::Sit)
    }

    fn go(&mut self, phase: Phase) {
        self.from = Some((self.shown, Instant::now()));
        self.phase = phase;
        self.started = Instant::now();
    }

    /// Move on when the time has come. An action is always finished before the cat lies down.
    fn advance(&mut self) {
        let t = self.started.elapsed().as_secs_f64();
        match self.phase {
            Phase::Asleep if self.hovered => self.go(Phase::Waking),
            Phase::Waking if !self.hovered => self.go(Phase::Settling),
            Phase::Waking if t >= WAKING => {
                // It has just yawned.
                let first = self.next_act(Act::Yawn);
                self.go(Phase::Awake(first));
            }
            Phase::Awake(act) if t >= act.seconds() => {
                if self.hovered {
                    let next = self.next_act(act);
                    self.go(Phase::Awake(next));
                } else {
                    self.go(Phase::Settling);
                }
            }
            Phase::Settling if self.hovered => self.go(Phase::Waking),
            Phase::Settling if t >= SETTLING => self.go(Phase::Asleep),
            _ => {}
        }
    }

    fn frame(&mut self, roam: f64) -> Pose {
        self.advance();
        let t = self.started.elapsed().as_secs_f64();
        let target = match self.phase {
            Phase::Asleep => asleep(t),
            Phase::Waking => waking(t),
            Phase::Awake(act) => pose(act, t, roam),
            Phase::Settling => settling(t),
        };
        let shown = match self.from {
            Some((from, at)) if at.elapsed().as_secs_f64() < CROSSFADE => crossfade(&from, &target, smooth(at.elapsed().as_secs_f64() / CROSSFADE)),
            _ => target,
        };
        self.shown = shown;
        shown
    }
}

// ── Drawing ───────────────────────────────────────────────────────────────

const COAT: Rgba = (0.075, 0.065, 0.10, 1.0);
const FAR_COAT: Rgba = (0.045, 0.04, 0.065, 1.0);
const RIM: Rgba = (0.60, 0.54, 0.72, 1.0);
const EAR_INSIDE: Rgba = (0.34, 0.17, 0.30, 1.0);
const PINK: Rgba = (0.98, 0.50, 0.66, 1.0);
const MOUTH: Rgba = (0.36, 0.07, 0.16, 1.0);
const AMBER_TOP: Rgba = (0.86, 0.44, 0.05, 1.0);
const AMBER_LOW: Rgba = (1.0, 0.84, 0.20, 1.0);
const WHITE: Rgba = (1.0, 1.0, 1.0, 1.0);
const PUPIL: Rgba = (0.035, 0.025, 0.05, 1.0);
const LINE: Rgba = (0.78, 0.74, 0.86, 1.0);

/// Two circles joined by their outer tangents: every limb and the torso.
fn capsule(cr: *mut Cairo, a: P, ra: f64, b: P, rb: f64) {
    let d = (b.0 - a.0).hypot(b.1 - a.1);
    unsafe {
        cairo_new_path(cr);
        if d <= (ra - rb).abs() {
            let (c, r) = if ra > rb { (a, ra) } else { (b, rb) };
            cairo_arc(cr, c.0, c.1, r, 0.0, TAU);
        } else {
            let along = (b.1 - a.1).atan2(b.0 - a.0);
            let spread = ((ra - rb) / d).clamp(-1.0, 1.0).acos();
            cairo_arc(cr, a.0, a.1, ra, along + spread, along - spread + TAU);
            cairo_arc(cr, b.0, b.1, rb, along - spread, along + spread);
            cairo_close_path(cr);
        }
        cairo_fill(cr);
    }
}

fn set(cr: *mut Cairo, c: Rgba) {
    unsafe { cairo_set_source_rgba(cr, c.0, c.1, c.2, c.3) };
}

fn faded(c: Rgba, a: f64) -> Rgba {
    (c.0, c.1, c.2, c.3 * a)
}

/// A leg from `top` to `paw`, its joint bent `bend` to one side.
fn leg(cr: *mut Cairo, top: P, top_r: f64, paw: P, r: f64, bend: f64, toe: f64, grow: f64) {
    let mid = ((top.0 + paw.0) / 2.0 + bend, (top.1 + paw.1) / 2.0);
    capsule(cr, top, top_r + grow, mid, r + 1.5 + grow);
    capsule(cr, mid, r + 1.5 + grow, paw, r + grow);
    unsafe {
        cairo_save(cr);
        cairo_translate(cr, paw.0 + 3.0 * toe, paw.1 + 1.0);
        cairo_scale(cr, lerp(1.1, 1.45, toe), 0.85);
        cairo_new_path(cr);
        cairo_arc(cr, 0.0, 0.0, r + 0.5 + grow, 0.0, TAU);
        cairo_fill(cr);
        cairo_restore(cr);
    }
}

/// 1 seen from the side, 0 facing the viewer.
fn profile(b: &Body) -> f64 {
    ((b.shoulder.0 - b.hip.0) / 30.0).clamp(0.0, 1.0)
}

/// The tail's start, two control points and tip.
fn tail_curve(b: &Body) -> [P; 4] {
    // The root climbs the rump as the tail rises, from low at the back to the top of the hips.
    let raised = ((b.tail - PI) / FRAC_PI_2).clamp(0.0, 1.0).sqrt();
    let root = lerp(2.54, 4.3, raised);
    // Seen from the front it comes from the middle of the back.
    let side = profile(b);
    let from = (b.hip.0 + b.hip_r * 0.82 * root.cos() * side, b.hip.1 + b.hip_r * 0.82 * root.sin());
    let (dx, dy) = (b.tail.cos(), b.tail.sin());
    let (nx, ny) = (-dy, dx);
    let at = |along: f64, across: f64| (from.0 + dx * along + nx * across * b.curl, from.1 + dy * along + ny * across * b.curl);
    [from, at(26.0, -10.0), at(50.0, 16.0), at(66.0, 34.0)]
}

fn tail(cr: *mut Cairo, b: &Body, width: f64) {
    let [from, c1, c2, end] = tail_curve(b);
    unsafe {
        cairo_set_line_cap(cr, 1);
        cairo_set_line_width(cr, width);
        cairo_new_path(cr);
        cairo_move_to(cr, from.0, from.1);
        cairo_curve_to(cr, c1.0, c1.1, c2.0, c2.1, end.0, end.1);
        cairo_stroke(cr);
    }
}

/// The ear on `side`, turned by a flick.
unsafe fn in_ear(cr: *mut Cairo, side: f64, ear: f64, draw: impl FnOnce()) {
    unsafe {
        cairo_save(cr);
        cairo_translate(cr, side * 20.0, -14.0);
        cairo_rotate(cr, side * if side > 0.0 { ear * 0.28 } else { 0.0 });
        draw();
        cairo_restore(cr);
    }
}

/// Face and ears.
fn head_shape(cr: *mut Cairo, ear: f64, grow: f64) {
    unsafe {
        for side in [-1.0, 1.0] {
            in_ear(cr, side, ear, || {
                cairo_new_path(cr);
                cairo_move_to(cr, side * -15.0, 6.0);
                cairo_line_to(cr, side * 8.0, -25.0);
                cairo_line_to(cr, side * 14.0, 10.0);
                cairo_close_path(cr);
                cairo_set_line_join(cr, 1);
                cairo_set_line_width(cr, 7.0 + 2.0 * grow);
                cairo_fill_preserve(cr);
                cairo_stroke(cr);
            });
        }
        cairo_save(cr);
        cairo_scale(cr, 1.24, 0.96);
        cairo_new_path(cr);
        cairo_arc(cr, 0.0, 0.0, 29.0 + grow, 0.0, TAU);
        cairo_fill(cr);
        cairo_restore(cr);
    }
}

/// A four-pointed sparkle.
fn sparkle(cr: *mut Cairo, x: f64, y: f64, r: f64) {
    unsafe {
        set(cr, WHITE);
        cairo_new_path(cr);
        cairo_move_to(cr, x, y - r);
        cairo_curve_to(cr, x, y - r * 0.2, x + r * 0.2, y, x + r, y);
        cairo_curve_to(cr, x + r * 0.2, y, x, y + r * 0.2, x, y + r);
        cairo_curve_to(cr, x, y + r * 0.2, x - r * 0.2, y, x - r, y);
        cairo_curve_to(cr, x - r * 0.2, y, x, y - r * 0.2, x, y - r);
        cairo_fill(cr);
    }
}

/// A big amber eye with a white rim and sparkle, or a closed curve.
fn eye(cr: *mut Cairo, x: f64, y: f64, open: f64, look: f64) {
    const R: f64 = 12.5;
    unsafe {
        if open <= 0.12 {
            set(cr, AMBER_LOW);
            cairo_set_line_cap(cr, 1);
            cairo_set_line_width(cr, 2.8);
            cairo_new_path(cr);
            cairo_arc(cr, x, y - 4.0, 8.0, 0.2 * PI, 0.8 * PI);
            cairo_stroke(cr);
            return;
        }
        cairo_save(cr);
        cairo_translate(cr, x, y);
        cairo_scale(cr, 1.0, open.min(1.0));
        fill_circle(cr, 0.0, 0.0, R + 1.3, WHITE);
        let iris = cairo_pattern_create_linear(0.0, -R, 0.0, R);
        cairo_pattern_add_color_stop_rgba(iris, 0.0, AMBER_TOP.0, AMBER_TOP.1, AMBER_TOP.2, 1.0);
        cairo_pattern_add_color_stop_rgba(iris, 1.0, AMBER_LOW.0, AMBER_LOW.1, AMBER_LOW.2, 1.0);
        cairo_set_source(cr, iris);
        cairo_new_path(cr);
        cairo_arc(cr, 0.0, 0.0, R, 0.0, TAU);
        cairo_fill(cr);
        cairo_pattern_destroy(iris);
        fill_circle(cr, look * 2.5, 0.5, R * 0.68, PUPIL);
        if open > 0.5 {
            sparkle(cr, 3.2 + look * 2.5, -3.6, 5.0);
            fill_circle(cr, -3.8 + look * 2.5, 4.2, 1.7, WHITE);
        }
        cairo_restore(cr);
    }
}

fn face(cr: *mut Cairo, p: &Pose) {
    let lx = p.look * 6.0;
    unsafe {
        set(cr, EAR_INSIDE);
        for side in [-1.0, 1.0] {
            in_ear(cr, side, p.ear, || {
                cairo_new_path(cr);
                cairo_move_to(cr, side * -7.0, 2.0);
                cairo_line_to(cr, side * 7.0, -16.0);
                cairo_line_to(cr, side * 9.0, 5.0);
                cairo_close_path(cr);
                cairo_fill(cr);
            });
        }
        for side in [-1.0, 1.0] {
            eye(cr, lx + side * 15.0, -3.0, p.eyes, p.look);
        }
        set(cr, PINK);
        cairo_new_path(cr);
        cairo_move_to(cr, lx - 3.0, 9.0);
        cairo_line_to(cr, lx + 3.0, 9.0);
        cairo_line_to(cr, lx, 12.5);
        cairo_close_path(cr);
        cairo_fill(cr);
        if p.mouth > 0.04 {
            set(cr, MOUTH);
            cairo_save(cr);
            cairo_translate(cr, lx, 18.0);
            cairo_scale(cr, 0.85, p.mouth.max(0.2));
            cairo_new_path(cr);
            cairo_arc(cr, 0.0, 0.0, 9.0, 0.0, TAU);
            cairo_fill(cr);
            cairo_restore(cr);
            fill_circle(cr, lx, 18.0 + 4.6 * p.mouth, 4.2 * p.mouth, PINK);
        } else {
            set(cr, faded(LINE, 0.75));
            cairo_set_line_cap(cr, 1);
            cairo_set_line_width(cr, 1.7);
            for side in [-1.0, 1.0] {
                cairo_new_path(cr);
                cairo_arc(cr, lx + side * 3.2, 13.0, 3.2, 0.1 * PI, 0.9 * PI);
                cairo_stroke(cr);
            }
        }
        if p.tongue > 0.05 {
            fill_circle(cr, lx + 2.0, 18.5 + 3.0 * p.tongue, 3.2, PINK);
        }
        set(cr, faded(LINE, 0.4));
        cairo_set_line_width(cr, 1.1);
        for side in [-1.0, 1.0] {
            for (dy, fan) in [(-2.0, -3.0), (2.0, 1.0)] {
                cairo_new_path(cr);
                cairo_move_to(cr, lx + side * 24.0, 11.0 + dy);
                cairo_line_to(cr, lx + side * 42.0, 11.0 + dy + fan);
                cairo_stroke(cr);
            }
        }
    }
}

/// The whole cat in the given colours, grown by `grow`.
fn silhouette(cr: *mut Cairo, p: &Pose, far: Rgba, near: Rgba, grow: f64) {
    let b = &p.body;
    // Facing the viewer the legs come straight down on either side, with no knee to the front.
    let side = profile(b);
    let hip_at = |sideways: f64, front: f64| (b.hip.0 + lerp(front, sideways, side), b.hip.1 + 6.0);
    set(cr, near);
    tail(cr, b, 13.0 + 2.0 * grow);
    set(cr, far);
    leg(cr, (b.shoulder.0 - lerp(10.0, 9.0, side), b.shoulder.1 + 8.0), 11.0, b.fore_far, 6.5, 3.0 * side, side, grow);
    leg(cr, hip_at(10.0, 15.0), 13.0, b.hind_far, 6.5, 8.0 * side, side, grow);
    set(cr, near);
    leg(cr, hip_at(-2.0, -15.0), 15.0, b.hind, 7.0, 10.0 * side, side, grow);
    capsule(cr, b.hip, b.hip_r + grow, b.shoulder, b.shoulder_r + grow);
    leg(cr, (b.shoulder.0 + lerp(10.0, 4.0, side), b.shoulder.1 + 8.0), 12.0, b.fore, 7.0, 3.0 * side, side, grow);
    unsafe {
        cairo_save(cr);
        cairo_translate(cr, b.head.0, b.head.1);
        cairo_rotate(cr, p.tilt);
        head_shape(cr, p.ear, grow);
        cairo_restore(cr);
    }
}

fn draw_yarn(cr: *mut Cairo, x: f64, rolled: f64, size: f64, look: &Look) {
    const R: f64 = 15.0;
    let wool = (look.primary.0, look.primary.1, look.primary.2, 1.0);
    let strand = (wool.0 * 0.7 + 0.3, wool.1 * 0.7 + 0.3, wool.2 * 0.7 + 0.3, 1.0);
    unsafe {
        cairo_save(cr);
        cairo_translate(cr, x, -R * size);
        cairo_scale(cr, size, size);
        // The loose end trailing along the ground.
        set(cr, strand);
        cairo_set_line_cap(cr, 1);
        cairo_set_line_width(cr, 2.2);
        cairo_new_path(cr);
        cairo_move_to(cr, -4.0, R - 1.0);
        cairo_curve_to(cr, -14.0, R + 4.0, -20.0, R - 3.0, -28.0, R - 0.5);
        cairo_stroke(cr);
        cairo_rotate(cr, rolled);
        fill_circle(cr, 0.0, 0.0, R, wool);
        set(cr, strand);
        cairo_set_line_width(cr, 1.8);
        for (angle, bow) in [(0.0, 6.0), (1.1, 8.0), (2.2, 5.0)] {
            cairo_save(cr);
            cairo_rotate(cr, angle);
            for side in [-1.0, 1.0] {
                cairo_new_path(cr);
                cairo_move_to(cr, -R * 0.9, side * 3.0);
                cairo_curve_to(cr, -R * 0.3, side * bow * 1.6, R * 0.3, side * bow * 1.6, R * 0.9, side * 3.0);
                cairo_stroke(cr);
            }
            cairo_restore(cr);
        }
        cairo_restore(cr);
    }
}

fn draw_butterfly(cr: *mut Cairo, x: f64, y: f64, beat: f64, show: f64) {
    let wing = faded(AMBER_LOW, show);
    unsafe {
        cairo_save(cr);
        cairo_translate(cr, x, y);
        for side in [-1.0, 1.0] {
            cairo_save(cr);
            cairo_scale(cr, side * (0.3 + 0.7 * beat), 1.0);
            set(cr, wing);
            cairo_new_path(cr);
            cairo_move_to(cr, 0.0, 0.0);
            cairo_curve_to(cr, 4.0, -13.0, 16.0, -12.0, 13.0, -2.0);
            cairo_curve_to(cr, 16.0, 7.0, 6.0, 10.0, 0.0, 0.0);
            cairo_fill(cr);
            cairo_restore(cr);
        }
        set(cr, faded(PUPIL, show));
        cairo_set_line_cap(cr, 1);
        cairo_set_line_width(cr, 2.2);
        cairo_new_path(cr);
        cairo_move_to(cr, 0.0, -5.0);
        cairo_line_to(cr, 0.0, 6.0);
        cairo_stroke(cr);
        cairo_restore(cr);
    }
}

fn draw_heart(cr: *mut Cairo, x: f64, y: f64, r: f64, alpha: f64) {
    unsafe {
        set(cr, faded(PINK, alpha));
        cairo_new_path(cr);
        cairo_move_to(cr, x, y + r);
        cairo_curve_to(cr, x - r * 1.7, y - r * 0.2, x - r * 0.8, y - r * 1.3, x, y - r * 0.35);
        cairo_curve_to(cr, x + r * 0.8, y - r * 1.3, x + r * 1.7, y - r * 0.2, x, y + r);
        cairo_fill(cr);
    }
}

fn draw_pose(cr: *mut Cairo, w: f64, h: f64, look: &Look, p: &Pose) {
    let k = (w / DESIGN_W).min(h / DESIGN_H);
    unsafe {
        cairo_save(cr);
        cairo_translate(cr, w / 2.0, (h + DESIGN_H * k) / 2.0 - GROUND * k);
        cairo_scale(cr, k, k);
        // A shadow on the ground, shrinking while the cat is in the air.
        let spread = 1.0 - p.lift / 140.0;
        cairo_save(cr);
        cairo_translate(cr, p.x, 0.0);
        cairo_scale(cr, 1.0, 0.09);
        fill_circle(cr, 0.0, 0.0, 74.0 * spread, (0.0, 0.0, 0.0, 0.3 * spread));
        cairo_restore(cr);

        cairo_translate(cr, p.x, 0.0);
        cairo_scale(cr, p.facing, 1.0);
        if let Some((x, rolled, size)) = p.yarn {
            draw_yarn(cr, x, rolled, size, look);
        }
        cairo_translate(cr, 0.0, -p.lift);

        // A fine light edge, so that the black cat reads on a dark wallpaper.
        cairo_push_group(cr);
        silhouette(cr, p, RIM, RIM, 1.3);
        cairo_pop_group_to_source(cr);
        cairo_paint_with_alpha(cr, 0.5);

        // The coat, with a soft sheen from above.
        cairo_push_group(cr);
        silhouette(cr, p, FAR_COAT, COAT, 0.0);
        let sheen = cairo_pattern_create_linear(0.0, -170.0, 0.0, -10.0);
        cairo_pattern_add_color_stop_rgba(sheen, 0.0, 0.42, 0.36, 0.56, 0.42);
        cairo_pattern_add_color_stop_rgba(sheen, 0.55, 0.20, 0.16, 0.30, 0.16);
        cairo_pattern_add_color_stop_rgba(sheen, 1.0, 0.0, 0.0, 0.0, 0.2);
        cairo_set_operator(cr, CAIRO_OPERATOR_ATOP);
        cairo_set_source(cr, sheen);
        cairo_paint(cr);
        cairo_pattern_destroy(sheen);
        cairo_pop_group_to_source(cr);
        cairo_paint(cr);

        cairo_save(cr);
        cairo_translate(cr, p.body.head.0, p.body.head.1);
        cairo_rotate(cr, p.tilt);
        face(cr, p);
        cairo_restore(cr);

        if let Some(t) = p.zzz {
            for i in 0..3 {
                let life = (t / 3.6 + i as f64 / 3.0).rem_euclid(1.0);
                let size = 13.0 + 15.0 * life;
                let font = format!("{} Bold {size:.0}px", look.family);
                let at = (p.body.head.0 + 22.0 + 28.0 * life, p.body.head.1 - 40.0 - 66.0 * life);
                let alpha = bump(life, 0.0, 1.0, 0.25);
                draw_pango_text(cr, at.0, at.1, "z", &font, faded(AMBER_LOW, alpha), None);
            }
        }
        if let Some(t) = p.hearts {
            for i in 0..4 {
                let life = (t / 1.5 + i as f64 / 4.0).rem_euclid(1.0);
                let side = if i % 2 == 0 { 1.0 } else { -1.0 };
                let x = p.body.head.0 + side * (24.0 + 14.0 * life) + 5.0 * (life * 9.0).sin();
                draw_heart(cr, x, p.body.head.1 - 34.0 - 56.0 * life, 5.5 + 3.5 * life, bump(life, 0.0, 1.0, 0.22));
            }
        }
        if let Some((x, y, beat, show)) = p.fly {
            // It flies on its own: the cat's leap does not carry it along.
            draw_butterfly(cr, x, y + p.lift, beat, show);
        }
        cairo_restore(cr);
    }
}

/// How far the cat can walk from the middle of a widget `w` by `h`.
fn roam(w: f64, h: f64) -> f64 {
    let k = (w / DESIGN_W).min(h / DESIGN_H);
    (w / k / 2.0 - 112.0).max(0.0)
}

/// Draw the cat as it is now.
pub fn draw(cr: *mut Cairo, w: f64, h: f64, look: &Look, state: &mut CatState) {
    let p = state.frame(roam(w, h));
    draw_pose(cr, w, h, look, &p);
}

/// The cat asleep, for edit mode.
pub fn draw_still(cr: *mut Cairo, w: f64, h: f64, look: &Look) {
    draw_pose(cr, w, h, look, &asleep(1.0));
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Left, right and top of everything a pose draws, in design units around the ground spot.
    fn reach(p: &Pose) -> (f64, f64, f64) {
        let b = &p.body;
        let mut pts: Vec<(f64, f64, f64)> = vec![(b.hip.0, b.hip.1, b.hip_r), (b.shoulder.0, b.shoulder.1, b.shoulder_r), (b.head.0, b.head.1 - 24.0, 44.0), (b.head.0, b.head.1, 44.0)];
        for paw in [b.fore, b.fore_far, b.hind, b.hind_far] {
            pts.push((paw.0 + 3.0, paw.1, 12.0));
        }
        for at in tail_curve(b) {
            pts.push((at.0, at.1, 8.0));
        }
        let mut out: Vec<(f64, f64)> = Vec::new();
        for (x, y, r) in pts {
            let x = p.x + p.facing * x;
            out.push((x - r, y - r - p.lift));
            out.push((x + r, y - r - p.lift));
        }
        if let Some((x, _, _)) = p.yarn {
            let x = p.x + p.facing * x;
            out.push((x - 30.0, -30.0));
            out.push((x + 30.0, -30.0));
        }
        if let Some((x, y, _, _)) = p.fly {
            let x = p.x + p.facing * x;
            out.push((x - 16.0, y - 14.0));
            out.push((x + 16.0, y - 14.0));
        }
        (out.iter().map(|q| q.0).fold(f64::MAX, f64::min), out.iter().map(|q| q.0).fold(f64::MIN, f64::max), out.iter().map(|q| q.1).fold(f64::MAX, f64::min))
    }

    #[test]
    fn nothing_ever_leaves_the_widget() {
        let range = roam(DESIGN_W, DESIGN_H);
        let check = |name: String, p: &Pose| {
            let (left, right, top) = reach(p);
            assert!(left >= -DESIGN_W / 2.0 && right <= DESIGN_W / 2.0, "{name}: {left} to {right}");
            assert!(top >= GROUND - DESIGN_H, "{name}: top {top}");
        };
        for act in Act::ALL {
            for i in 0..=(act.seconds() * 30.0) as usize {
                let t = i as f64 / 30.0;
                check(format!("{act:?} at {t}"), &pose(act, t, range));
            }
        }
        for i in 0..150 {
            let t = i as f64 / 30.0;
            check(format!("asleep at {t}"), &asleep(t));
            check(format!("waking at {t}"), &waking(t));
            check(format!("settling at {t}"), &settling(t));
        }
    }

    #[test]
    fn the_tail_only_ever_swings_over_the_back() {
        for b in [SIT, STAND, FRONT, REAR, STRETCH, CROUCH, LOAF] {
            assert!((PI..=3.0 * FRAC_PI_2 + 1e-9).contains(&b.tail), "{}", b.tail);
            assert!(b.curl >= 0.0);
        }
        // Halfway between sitting and standing it points up and back, never forward.
        let half = blend(&SIT, &STAND, 0.5);
        assert!(half.tail.cos() < 0.0 && half.tail.sin() < 0.0);
    }

    #[test]
    fn a_raised_tail_starts_high_on_the_rump() {
        let root = |b: &Body| tail_curve(b)[0].1 - b.hip.1;
        assert!(root(&SIT) > 0.0, "low at the back while sitting");
        assert!(root(&STAND) < -b_radius(&STAND) * 0.5, "high while walking");
        assert!(root(&STRETCH) < -b_radius(&STRETCH) * 0.5);
    }

    fn b_radius(b: &Body) -> f64 {
        b.hip_r
    }

    #[test]
    fn the_paw_reaches_the_butterfly() {
        for t in [BUTTERFLY_SWAT, BUTTERFLY_HIT] {
            let p = pose(Act::Butterfly, t, 0.0);
            let (fx, fy, _, show) = p.fly.unwrap();
            let gap = (p.body.fore.0 - fx).hypot(p.body.fore.1 - p.lift - fy);
            assert!(gap < 4.0, "paw {gap} from the butterfly at {t}");
            assert!(show > 0.9);
        }
        assert!(pose(Act::Butterfly, BUTTERFLY_HIT, 0.0).lift > 20.0, "the second swat is a leap");
    }

    #[test]
    fn walking_legs_never_cross_on_the_way_into_a_turn() {
        for i in 0..=20 {
            let b = blend(&STAND, &FRONT, i as f64 / 20.0);
            assert!(b.hind.0 < b.hind_far.0 && b.fore.0 > b.fore_far.0);
        }
    }

    #[test]
    fn head_movements_dart_rather_than_drift() {
        // Most of a step is held still; the change happens in a fraction of it.
        let (a, b) = (dart(0.4, 0.7, 9.0), dart(0.65, 0.7, 9.0));
        assert_eq!(a, b);
        let moved = (dart(0.7, 0.7, 9.0) - dart(0.7 + 0.13, 0.7, 9.0)).abs();
        assert!(moved > 0.01);
    }

    #[test]
    fn a_turn_is_one_quick_hop_and_the_walk_keeps_its_pace() {
        let frames = (Act::Walk.seconds() * 240.0) as usize;
        let turning = (0..frames).filter(|i| walk_path(*i as f64 / 240.0, 50.0).2 > 0.0).count() as f64 / 240.0;
        assert!(turning <= 2.0 * TURN + 0.02, "{turning} s turning");
        let leg = (Act::Walk.seconds() - 2.0 * WALK_BLEND - 2.0 * TURN) / 4.0;
        assert!((leg - 2.175).abs() < 1e-9, "same walking speed as before");
        // The feet stay where the stance puts them: only the hop lifts the cat.
        let mid = pose(Act::Walk, WALK_BLEND + leg + TURN / 2.0, 50.0);
        assert_eq!(mid.lift, TURN_HOP);
        for foot in [mid.body.fore, mid.body.fore_far, mid.body.hind, mid.body.hind_far] {
            assert_eq!(foot.1, FRONT.fore.1);
        }
    }

    #[test]
    fn a_turn_happens_facing_the_viewer() {
        let range = 50.0;
        let mut last = pose(Act::Walk, 0.0, range);
        let mut turns = 0;
        for i in 1..(Act::Walk.seconds() * 60.0) as usize {
            let p = pose(Act::Walk, i as f64 / 60.0, range);
            assert!(p.facing.abs() == 1.0, "never squeezed thin");
            if p.facing != last.facing {
                turns += 1;
                assert!((p.body.hip.0 - p.body.shoulder.0).abs() < 2.0, "turns only while front on");
            }
            last = p;
        }
        assert_eq!(turns, 2);
    }

    #[test]
    fn every_action_starts_and_ends_sitting() {
        for act in Act::ALL {
            for t in [0.0, act.seconds()] {
                let p = pose(act, t, 40.0);
                let d = (p.body.hip.0 - SIT.hip.0).hypot(p.body.hip.1 - SIT.hip.1) + (p.body.head.0 - SIT.head.0).hypot(p.body.head.1 - SIT.head.1);
                assert!(d < 0.5, "{act:?} at {t}: {d}");
                assert!(p.x.abs() < 0.5 && p.lift == 0.0 && p.facing == 1.0, "{act:?} at {t}");
            }
        }
    }

    #[test]
    fn waking_and_settling_join_sleep_and_sitting_without_a_jump() {
        let gap = |a: &Pose, b: &Pose| (a.body.hip.0 - b.body.hip.0).hypot(a.body.hip.1 - b.body.hip.1) + (a.body.head.0 - b.body.head.0).hypot(a.body.head.1 - b.body.head.1);
        assert!(gap(&waking(0.0), &asleep(0.0)) < 1.0);
        assert!(gap(&waking(WAKING), &sitting(WAKING)) < 1.0);
        assert!(gap(&settling(0.0), &sitting(0.0)) < 1.0);
        assert!(gap(&settling(SETTLING), &asleep(SETTLING)) < 1.5);
        assert!(settling(SETTLING).eyes < 0.05 && waking(WAKING).eyes > 0.9);
    }

    #[test]
    fn the_cat_sleeps_until_someone_hovers() {
        let mut cat = CatState::new();
        cat.frame(40.0);
        assert!(cat.resting());
        cat.hover(true);
        cat.frame(40.0);
        assert_eq!(cat.phase, Phase::Waking);
        assert!(!cat.resting());
        // Leaving while it wakes sends it straight back to bed.
        cat.hover(false);
        cat.frame(40.0);
        assert_eq!(cat.phase, Phase::Settling);
    }

    #[test]
    fn every_action_plays_before_any_plays_again() {
        let mut cat = CatState::new();
        let mut last = Act::Yawn;
        for _ in 0..50 {
            let mut round = Vec::new();
            for _ in 0..Act::ALL.len() {
                let next = cat.next_act(last);
                assert_ne!(next, last, "never twice in a row");
                round.push(next);
                last = next;
            }
            for act in Act::ALL {
                assert_eq!(round.iter().filter(|a| **a == act).count(), 1, "{act:?} in {round:?}");
            }
        }
    }

    #[test]
    fn the_rounds_come_in_different_orders() {
        let mut cat = CatState::new();
        let mut round = |last: Act| (0..Act::ALL.len()).map(|_| cat.next_act(last)).collect::<Vec<_>>();
        let first = round(Act::Yawn);
        assert!((0..8).any(|_| round(Act::Yawn) != first));
    }

    #[test]
    fn there_is_room_under_the_cat() {
        // The ground is well above the bottom of the widget, whatever its shape.
        for (w, h) in [(320.0, 290.0), (260.0, 220.0), (500.0, 300.0), (240.0, 400.0)] {
            let k = f64::min(w / DESIGN_W, h / DESIGN_H);
            let ground = (h + DESIGN_H * k) / 2.0 - GROUND * k;
            assert!(h - ground >= GROUND * k - 1e-9, "{w}x{h}");
        }
    }
}
