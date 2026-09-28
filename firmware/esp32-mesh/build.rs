//! ESP-IDF's build environment, plus the firmware's settings from `mesh.env`
//! (gitignored; see `mesh.env.example`), baked in at build time.

fn main() {
    embuild::espidf::sysenv::output();

    println!("cargo:rustc-check-cfg=cfg(mesh_ca)");
    println!("cargo:rerun-if-changed=mesh.env");
    let Ok(text) = std::fs::read_to_string("mesh.env") else {
        println!("cargo:warning=no mesh.env: copy mesh.env.example and fill it in");
        return;
    };
    for line in text.lines().map(str::trim) {
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if let Some((key, value)) = line.split_once('=') {
            let (key, value) = (key.trim(), value.trim().trim_matches('"'));
            if key == "MESH_CA_PEM_FILE" && !value.is_empty() {
                embed_ca(value);
            } else if key.chars().all(|c| c.is_ascii_uppercase() || c == '_') {
                println!("cargo:rustc-env={key}={value}");
            }
        }
    }
}

/// A private CA to trust for DERP (e.g. the mesh lab's, from `lab.sh
/// esp32-env`): copied next to the build and embedded with `include_bytes!`.
fn embed_ca(path: &str) {
    println!("cargo:rerun-if-changed={path}");
    let pem = std::fs::read(path).unwrap_or_else(|e| panic!("MESH_CA_PEM_FILE={path}: {e}"));
    let out = std::path::Path::new(&std::env::var("OUT_DIR").unwrap()).join("mesh_ca.pem");
    std::fs::write(&out, pem).unwrap();
    println!("cargo:rustc-env=MESH_CA_PEM_PATH={}", out.display());
    println!("cargo:rustc-cfg=mesh_ca");
}
