# `git-format-staged`

Modify staged files, backporting changes onto their unstaged versions.
Useful for formatting files as part of a [pre-commit hook](https://git-scm.com/book/en/v2/Customizing-Git-Git-Hooks).

Example:

```
$ git init

$ tree
.
├── a.txt
└── b
    └── c.txt

$ git add a.txt b/c.txt
                                     # Remove trailing whitespace
$ git-format-staged a.txt b/c.txt -- sed 's/\s\+$//' -i
```

## Installation

Try using Nix: `nix run github:LightAndLight/git-format-staged -- --help`

Add to a Nix Flake:

```nix
{
  inputs = {
    flake-utils.url = "github:numtide/flake-utils";
    git-format-staged.url = "github:LightAndLight/git-format-staged";
  };
  outputs = { self, nixpkgs, flake-utils, git-format-staged }:
    flake-utils.lib.eachDefaultSystem (system:
      let
        pkgs = import nixpkgs { inherit system; };
      in {
        devShell = pkgs.mkShell {
          buildInputs = with pkgs; [
            git-format-staged.packages.${system}.default
          ];
        };
      }
    );
}
```
