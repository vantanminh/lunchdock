use std::fs;
use std::path::Path;

fn main() {
    let dist = Path::new("../../web/dist");
    println!("cargo:rerun-if-changed=../../web/dist");
    let index = dist.join("index.html");
    if !index.exists() {
        let _ = fs::create_dir_all(dist);
        let _ = fs::write(
            index,
            "<!doctype html><html><head><title>Rudder</title></head><body><div id=\"root\"></div></body></html>\n",
        );
    }
}
