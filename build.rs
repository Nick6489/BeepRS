use std::{env, fs, path::PathBuf};

fn main() {
    let target = env::var("TARGET").unwrap();
    println!("cargo:rustc-env=BEEPRS_TARGET={target}");

    let sounds = [
        "beep.opus",
        "bed.opus",
        "intro.opus",
        "die1.opus",
        "die2.opus",
        "die3.opus",
    ];
    for name in sounds {
        println!("cargo:rerun-if-changed=sounds/{name}");
    }

    // Cargo's OUT_DIR is `<target>/<profile>/build/<pkg>/out`. The executable
    // is three directories above that, which is where a portable install expects
    // the replaceable sound files.
    let out_dir = PathBuf::from(env::var("OUT_DIR").unwrap());
    let beside_exe = out_dir
        .ancestors()
        .nth(3)
        .expect("unexpected OUT_DIR layout")
        .join("sounds");
    fs::create_dir_all(&beside_exe).unwrap();
    for name in sounds {
        fs::copy(format!("sounds/{name}"), beside_exe.join(name))
            .unwrap_or_else(|error| panic!("copy sounds/{name}: {error}"));
    }
}
