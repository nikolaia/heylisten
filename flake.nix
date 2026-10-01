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
          packages = with pkgs; [ cargo rustc rustfmt clippy rust-analyzer cmake pkg-config ];
          # whisper-rs-sys runs bindgen.
          LIBCLANG_PATH = "${pkgs.llvmPackages.libclang.lib}/lib";
        };
      });
    };
}
