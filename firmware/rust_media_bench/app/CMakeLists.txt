# Times devkit_media against media_framework on the same files.
if(ESP_PLATFORM)
    set(vfs vfs)
endif()
idf_component_register(
    SRCS "bench_files.c"
    INCLUDE_DIRS "."
    PRIV_REQUIRES devkit devkit_media idf media_framework rust_rt ${vfs})

if(NOT DEFINED PYTHON)
    set(PYTHON python3)
endif()

devkit_rust_out_dir(out_dir)
set(assets)
foreach(ext avi flac mkv mp3 mp4 opus wav)
    list(APPEND assets "${out_dir}/bench.${ext}")
endforeach()
add_custom_command(
    OUTPUT ${assets}
    COMMAND ${PYTHON} "${CMAKE_CURRENT_SOURCE_DIR}/gen_assets.py" "${out_dir}"
    DEPENDS "${CMAKE_CURRENT_SOURCE_DIR}/gen_assets.py"
    VERBATIM)

devkit_rust_crate(ROOT "lib.rs"
    BINDGEN_HEADERS "bench_files.h" "medf_buffer.h" "medf_tags.h" "medf_riff.h" "medf_avi.h" "medf_wav.h"
                    "medf_es.h" "medf_flac.h" "medf_ogg.h" "medf_mkv.h" "medf_mp4.h"
    DEPENDS ${assets})
