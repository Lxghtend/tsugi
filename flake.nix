{
  description = "tsugi — declarative Niri session switching";
  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";

  outputs = { self, nixpkgs }:
    let
      eachSystem = nixpkgs.lib.genAttrs [ "x86_64-linux" "aarch64-linux" ];
      manifest = builtins.fromTOML (builtins.readFile ./Cargo.toml);
    in {
      packages = eachSystem (system:
        let pkgs = nixpkgs.legacyPackages.${system}; in rec {
          default = tsugi;
          tsugi = pkgs.rustPlatform.buildRustPackage {
            pname = manifest.package.name;
            inherit (manifest.package) version;
            src = pkgs.lib.cleanSource self;
            cargoLock.lockFile = ./Cargo.lock;
            meta.mainProgram = "tsugi";
          };
        });
      devShells = eachSystem (system:
        let pkgs = nixpkgs.legacyPackages.${system}; in {
          default = pkgs.mkShell {
            packages = with pkgs; [ cargo rustc rustfmt clippy ];
          };
        });
      homeManagerModules.default = { config, lib, pkgs, ... }:
        let
          cfg = config.programs.tsugi;
          format = pkgs.formats.toml {};
        in {
          options.programs.tsugi = {
            enable = lib.mkEnableOption "tsugi";
            package = lib.mkOption {
              type = lib.types.package;
              default = self.packages.${pkgs.stdenv.hostPlatform.system}.tsugi;
            };
            profiles = lib.mkOption {
              type = lib.types.attrsOf format.type;
              default = {};
              description = "Profile tables, keyed by profile name.";
            };
          };
          config = lib.mkIf cfg.enable {
            assertions = lib.mapAttrsToList (name: profile: {
              assertion = builtins.match "[A-Za-z0-9_-]+" name != null && builtins.isAttrs profile;
              message = "Invalid tsugi profile name or table: ${name}";
            }) cfg.profiles;
            home.packages = [ cfg.package ];
            xdg.configFile = lib.mapAttrs' (name: profile:
              lib.nameValuePair "tsugi/profiles/${name}.toml" {
                source = format.generate "tsugi-${name}.toml" (profile // { inherit name; });
              }) cfg.profiles;
          };
        };
    };
}
