{
    inputs = {
        nixpkgs.url = "github:nixos/nixpkgs/nixos-unstable";

        rust-overlay = {
            url = "github:oxalica/rust-overlay";
            inputs.nixpkgs.follows = "nixpkgs";
        };
    };

    outputs = { nixpkgs, rust-overlay, ... }:
    let
        forAllSystems = f: nixpkgs.lib.genAttrs nixpkgs.lib.systems.flakeExposed f;
    in {
        devShells = forAllSystems (system: {
            default = nixpkgs.legacyPackages.${system}.mkShell (
            let
                pkgsCross = nixpkgs.legacyPackages.x86_64-linux.pkgsCross.aarch64-multiplatform;
                rust-bin = rust-overlay.lib.mkRustBin { } pkgsCross.buildPackages;
                cc = pkgsCross.lib.getExe pkgsCross.pkgsStatic.stdenv.cc;
            in {
                packages = [
                    (rust-bin.stable.latest.default.override {
                        targets = [ "aarch64-unknown-linux-musl" ];
                    })
                ];
                env = {
                    CC_aarch64_unknown_linux_musl = cc;
                    CARGO_TARGET_AARCH64_UNKNOWN_LINUX_MUSL_LINKER = cc;
                };
            });
        });
    };
}
