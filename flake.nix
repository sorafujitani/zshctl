{
  description = "zshctl, a Rust-native shell workflow suite for Zsh";

  inputs.nixpkgs.url = "https://flakehub.com/f/DeterminateSystems/nixpkgs-weekly/0.1";

  outputs = { self, nixpkgs }:
    let
      systems = [
        "aarch64-darwin"
        "aarch64-linux"
        "x86_64-linux"
      ];
      forAllSystems = nixpkgs.lib.genAttrs systems;
    in {
      packages = forAllSystems (system:
        let
          pkgs = import nixpkgs { inherit system; };
          version = "0.1.0";
          zshctl = pkgs.rustPlatform.buildRustPackage {
            pname = "zshctl";
            inherit version;
            src = ./.;

            cargoLock = {
              lockFile = ./Cargo.lock;
            };

            postInstall = ''
              install -Dm644 zshctl.zsh "$out/share/zshctl/zshctl.zsh"
              cp -R shells docs spec scripts "$out/share/zshctl/"
            '';

            meta = {
              description = "Rust-native stateful shell workflows for Zsh";
              homepage = "https://github.com/sorafujitani/zshctl";
              license = pkgs.lib.licenses.mit;
              mainProgram = "zshctl";
              platforms = systems;
            };
          };
        in {
          default = zshctl;
          zshctl = zshctl;
          zshctl-core = zshctl;
        });

      apps = forAllSystems (system:
        let
          zshctl = self.packages.${system}.zshctl-core;
        in {
          default = {
            type = "app";
            program = "${zshctl}/bin/zshctl";
            meta.description = "Run zshctl from the Nix package";
          };
          zshctl = {
            type = "app";
            program = "${zshctl}/bin/zshctl";
            meta.description = "Run zshctl from the Nix package";
          };
        });

      devShells = forAllSystems (system:
        let
          pkgs = import nixpkgs { inherit system; };
        in {
          default = pkgs.mkShell {
            packages = [
              pkgs.cargo
              pkgs.fzf
              pkgs.ghq
              pkgs.git
              pkgs.jq
              pkgs.rustc
              pkgs.sqlite
              pkgs.zsh
            ];
          };
        });
    };
}
