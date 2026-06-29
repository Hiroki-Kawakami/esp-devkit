# Shared epd_tweak app component. Picks up new .c/.cpp files on cmake re-run
# (CONFIGURE_DEPENDS isn't usable here -- IDF evaluates this file in script mode
# for requirement extraction, where it's invalid).
file(GLOB_RECURSE APP_SRCS
     "${CMAKE_CURRENT_SOURCE_DIR}/*.c"
     "${CMAKE_CURRENT_SOURCE_DIR}/*.cpp")

idf_component_register(
    SRCS ${APP_SRCS}
    INCLUDE_DIRS "."
    REQUIRES bsp ui_framework image_framework)
