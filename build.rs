use std::{env, fs, path::PathBuf};

fn main() {
    compile_resources();
    let target = env::var("TARGET").unwrap();
    println!("cargo:rustc-env=BEEPRS_TARGET={target}");

    let sounds = [
        "beep.opus",
        "bed.opus",
        "intro.opus",
        "die1.opus",
        "die2.opus",
        "die3.opus",
        "die4.opus",
        "die5.opus",
        "die6.opus",
        "die7.opus",
        "die8.opus",
        "die9.opus",
        "die10.opus",
        "die11.opus",
        "die12.opus",
        "die13.opus",
        "die14.opus",
        "die15.opus",
        "die16.opus",
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

fn compile_resources() {
    use std::process::Command;
    println!("cargo:rerun-if-changed=src/update.rc");
    println!("cargo:rerun-if-changed=src/update.manifest");
    println!("cargo:rerun-if-env-changed=RC");
    let output = PathBuf::from(env::var_os("OUT_DIR").unwrap()).join("update.res");
    let rc = env::var_os("RC").map(PathBuf::from).unwrap_or_else(|| {
        let kits =
            PathBuf::from(env::var_os("ProgramFiles(x86)").expect("Windows SDK is required"))
                .join("Windows Kits/10/bin");
        let mut versions: Vec<_> = fs::read_dir(kits)
            .expect("install Windows SDK or set RC")
            .filter_map(Result::ok)
            .map(|entry| entry.path().join("x64/rc.exe"))
            .filter(|path| path.is_file())
            .collect();
        versions.sort();
        versions
            .pop()
            .expect("install Windows SDK or set RC to rc.exe")
    });
    let status = Command::new(rc)
        .arg("/nologo")
        .arg("/fo")
        .arg(&output)
        .arg("src/update.rc")
        .status()
        .expect("run Windows resource compiler");
    assert!(
        status.success(),
        "Windows dialog resource compilation failed"
    );
    println!("cargo:rustc-link-arg-bin=beeprs={}", output.display());
    println!("cargo:rustc-link-arg-bin=beeprs=/MANIFEST:NO");
}
