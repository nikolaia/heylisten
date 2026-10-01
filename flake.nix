{
  description = "heyListen: local-only meeting transcription";

  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixpkgs-unstable";

  outputs = { nixpkgs, ... }:
    let
      systems = [ "aarch64-darwin" "x86_64-linux" "aarch64-linux" ];
      forAll = f: nixpkgs.lib.genAttrs systems (system: f nixpkgs.legacyPackages.${system});
    in
    {
      devShells = forAll (pkgs: {
        default = pkgs.mkShell {
          packages = with pkgs; [ cargo rustc rustfmt clippy rust-analyzer cmake pkg-config ]
            # Mic capture (cpal) and the tray app (GTK + AppIndicator) on Linux.
            ++ lib.optionals stdenv.hostPlatform.isLinux [ alsa-lib gtk3 libayatana-appindicator xdotool ];
          # whisper-rs-sys runs bindgen.
          LIBCLANG_PATH = "${pkgs.llvmPackages.libclang.lib}/lib";
        };
      });
    };
}
