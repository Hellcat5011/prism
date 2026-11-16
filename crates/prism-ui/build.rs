fn main() {
    println!("cargo:rustc-link-lib=cairo");
    println!("cargo:rustc-link-lib=pangocairo-1.0");
    println!("cargo:rustc-link-lib=pango-1.0");
    println!("cargo:rustc-link-lib=gobject-2.0");
    println!("cargo:rustc-link-lib=gdk_pixbuf-2.0");
    println!("cargo:rustc-link-lib=gdk-3");
}
