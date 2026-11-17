// The web UI is embedded from web/dist. Make sure the folder exists so the
// server builds before the UI has been built (it then serves a placeholder).
fn main() {
    let dist = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../web/dist");
    if !dist.join("index.html").exists() {
        std::fs::create_dir_all(&dist).unwrap();
        std::fs::write(
            dist.join("index.html"),
            "<!doctype html><title>KodoKuru</title><p>The web UI has not been built. Run <code>npm ci &amp;&amp; npm run build</code> in <code>web/</code>, then rebuild KodoKuru.</p>",
        )
        .unwrap();
    }
    println!("cargo:rerun-if-changed=../../web/dist");
}
