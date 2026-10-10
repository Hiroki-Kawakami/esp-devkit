# esp-devkit
Common components for ESP32 series development

## Usage

From an external project, `include(<path-to-esp-devkit>/devkit.cmake)` and use
the macros it defines. `project()` must stay literal in the wrapper file (not
inside a macro), since CMake pre-scans the top-level CMakeLists.txt for it.

Device wrapper (`<target>/CMakeLists.txt`):
```cmake
cmake_minimum_required(VERSION 3.16)
include(<path-to-esp-devkit>/devkit.cmake)
devkit_idf_init(COMPONENT_DIRS ../app)
project(my_app)
```
`devkit_idf_init` takes `COMPONENT_DIRS` (extra `EXTRA_COMPONENT_DIRS`, e.g.
the app component). Like `EXTRA_COMPONENT_DIRS`, each entry is either a
component or a directory whose subdirectories are components; the simulator
follows the same rule. The build is trimmed to whatever `main` transitively
`REQUIRES`.

Simulator wrapper (`simulator/CMakeLists.txt`):
```cmake
cmake_minimum_required(VERSION 3.16)
include(<path-to-esp-devkit>/devkit.cmake)
devkit_simulator_init()
project(simulator C CXX)
devkit_simulator()
```
The BSP board is selected with `CONFIG_BSP_BOARD_*` in the simulator's own
`sdkconfig` or `sdkconfig.defaults`.

`devkit_simulator` args: `DEFAULT_ROTATION`, `MAIN_SRCS`,
`COMPONENT_DIRS`, `SDKCONFIG`, `SDKCONFIG_DEFAULTS`, and
`SAVEDEFCONFIG`. The configuration paths default to `sdkconfig` and
`sdkconfig.defaults` in the simulator wrapper directory.

Shared components can expose compile-time options in their `Kconfig` file.
Simulator builds use their own sdkconfig, independently of the ESP-IDF target:
```sh
cmake --build build --target menuconfig
cmake --build build --target save-defconfig
```
The generated `sdkconfig.h` is on the simulator include path, and the generated
`CONFIG_*` variables are available while evaluating component CMakeLists.txt
files.

## Documentation

- [Test harness](docs/harness.md) — scripted UI verification: synthetic touch
  and button input plus a JPEG capture of the panel, driven by the same script
  on the simulator or a board.
- [resgen](docs/resgen.md) — build-time generator for LVGL fonts, icon fonts,
  images and the compressed font packs drawn by `PackedFont`.
- [CI firmware build](docs/ci.md) — GitHub Actions build of the firmware in a
  Nix-built image of the devShell's IDF environment.
