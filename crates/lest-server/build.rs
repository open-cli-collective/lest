// The UI is embedded from ui/dist. Without a built UI (a Rust-only build or
// CI job), embed a page that says how to build it.
fn main() {
    let dist = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../ui/dist");
    if !dist.join("index.html").exists() {
        std::fs::create_dir_all(&dist).expect("create ui/dist");
        std::fs::write(
            dist.join("index.html"),
            "<!doctype html><meta charset=utf-8><title>Lest</title><p style=\"font:16px system-ui;padding:40px\">The Lest UI was not built into this binary. Run <code>npm ci &amp;&amp; npm run build</code> in <code>ui/</code>, then rebuild.</p>",
        )
        .expect("write placeholder");
    }
    println!("cargo:rerun-if-changed=../../ui/dist");
}
