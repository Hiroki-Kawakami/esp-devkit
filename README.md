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
`devkit_idf_init` takes `COMPONENT_DIRS` (extra `EXTRA_COMPONENT_DIRS`, e.g. the
app component), registers every devkit device component alongside them, and
trims the build to whatever `main` (transitively) `REQUIRES`.

Simulator wrapper (`simulator/CMakeLists.txt`):
```cmake
cmake_minimum_required(VERSION 3.16)
include(<path-to-esp-devkit>/devkit.cmake)
devkit_simulator_init()
project(simulator C CXX)
devkit_simulator(BOARD airq)
```
`devkit_simulator` args: `BOARD` (required), `LV_CONF_DIR`, `DEFAULT_ROTATION`,
`MAIN_SRCS`, `COMPONENT_DIRS`.
