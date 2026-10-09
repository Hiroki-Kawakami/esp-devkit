# CI firmware build

Two composite actions build firmware on GitHub Actions in the same IDF
environment `nix develop` gives locally:

| piece | what it does |
|---|---|
| `lib.<system>.mkFwImage` (`flake.nix`) | the devShell's IDF environment (all toolchains, patched IDF) as a `streamLayeredImage`; `extraPackages` and `extraEnv` add project tools |
| `ci/image` | builds the image inside `nixos/nix` and pushes it, unless the tag already exists |
| `ci/firmware` | runs `idf.py -C <project-dir> build merge-bin` in the image and uploads `merged-binary.bin`, the app ELF and `sdkconfig` |

The project exposes the image from its flake and calls the actions by local
path, so they always match the esp-devkit commit the submodule points at:

```nix
packages.fw-image = esp-devkit.lib.${system}.mkFwImage {
  extraPackages = [ myTool ];
  extraEnv.MY_TOOL = "${myTool}/bin/my-tool";
};
```

```yaml
jobs:
  image:
    permissions: { contents: read, packages: write }
    outputs:
      image-ref: ${{ steps.image.outputs.image-ref }}
    steps:
      - uses: actions/checkout@v7
        with: { submodules: true, fetch-depth: 0 }
      - id: image
        uses: ./esp-devkit/ci/image
  firmware:
    needs: image
    permissions: { contents: read, packages: read }
    steps:
      - uses: actions/checkout@v7
        with: { submodules: true }
      - uses: ./esp-devkit/ci/firmware
        with:
          image-ref: ${{ needs.image.outputs.image-ref }}
          project-dir: esp32p4
```

The image tag is a hash of the project's `flake.lock` without local path inputs,
esp-devkit's `flake.nix` and `nix/`, and the project's `key-files` (default
`flake.nix`). The esp-devkit rev is left out so that bumping the submodule for
library changes reuses the image. Anything else the image depends on must be
listed in `key-files`, or run with `force: true`.

The build runs with `docker run` rather than `jobs.<id>.container`: Actions
injects its glibc-linked Node into job containers, which cannot start in a Nix
image with no `/lib64` loader.
