//! Fetches the diarization models at build time so they can be embedded in the binary.
//! heyListen itself never downloads anything (see docs/adr/0002).

use std::path::Path;
use std::process::Command;

const RELEASES: &str = "https://github.com/k2-fsa/sherpa-onnx/releases/download";

fn main() {
    let out = std::env::var("OUT_DIR").unwrap();
    let out = Path::new(&out);

    // pyannote segmentation 3.0 (MIT), shipped by sherpa-onnx as a tarball.
    let segmentation = out.join("segmentation.onnx");
    if !verified(&segmentation, "220ad67ca923bef2fa91f2390c786097bf305bceb5e261d4af67b38e938e1079") {
        let tarball = out.join("segmentation.tar.bz2");
        download(&format!("{RELEASES}/speaker-segmentation-models/sherpa-onnx-pyannote-segmentation-3-0.tar.bz2"), &tarball);
        run(Command::new("tar").arg("xjf").arg(&tarball).arg("-C").arg(out));
        std::fs::rename(out.join("sherpa-onnx-pyannote-segmentation-3-0/model.onnx"), &segmentation).unwrap();
        assert!(verified(&segmentation, "220ad67ca923bef2fa91f2390c786097bf305bceb5e261d4af67b38e938e1079"));
    }

    // 3D-Speaker CAM++ speaker embeddings, Chinese + English (Apache-2.0).
    let embedding = out.join("embedding.onnx");
    if !verified(&embedding, "aa3cfc16963a10586a9393f5035d6d6b57e98d358b347f80c2a30bf4f00ceba2") {
        download(
            &format!("{RELEASES}/speaker-recongition-models/3dspeaker_speech_campplus_sv_zh_en_16k-common_advanced.onnx"),
            &embedding,
        );
        assert!(verified(&embedding, "aa3cfc16963a10586a9393f5035d6d6b57e98d358b347f80c2a30bf4f00ceba2"));
    }
    println!("cargo:rerun-if-changed=build.rs");
}

fn download(url: &str, dest: &Path) {
    run(Command::new("curl").args(["-fsSL", "-o"]).arg(dest).arg(url));
}

fn verified(path: &Path, sha256: &str) -> bool {
    if !path.exists() {
        return false;
    }
    let tool = if cfg!(target_os = "macos") { vec!["shasum", "-a", "256"] } else { vec!["sha256sum"] };
    let output = Command::new(tool[0]).args(&tool[1..]).arg(path).output().unwrap();
    String::from_utf8_lossy(&output.stdout).starts_with(sha256)
}

fn run(cmd: &mut Command) {
    let status = cmd.status().unwrap_or_else(|e| panic!("can't run {cmd:?}: {e}"));
    assert!(status.success(), "{cmd:?} failed");
}
