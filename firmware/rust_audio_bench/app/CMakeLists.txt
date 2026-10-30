# Times devkit_audio against audio_framework on the same clips.
idf_component_register(
    SRCS "/dev/null"
    PRIV_REQUIRES audio_framework devkit devkit_audio idf rust_rt)

if(NOT DEFINED ENV{AUDIO_BENCH_CLIPS})
    message(FATAL_ERROR "AUDIO_BENCH_CLIPS must name a directory from app/make_clips.py")
endif()

devkit_rust_out_dir(out_dir)
set(clips "")
foreach(clip mp3_l3_44_128mp3_2 mp3_l3_44_320mp3_2 mp3_l2_44_256mp2_2 aac_adts_lc_44_2_128_he0
             aac_adts_he_48_2_64_he1 aac_adts_ps_48_2_32_he2 opus_music_128k opus_speech_16k_stereo
             opus_speech_8k flac_s16_44_l5 flac_s24_96_l5 alac_ff_s16_44)
    configure_file("$ENV{AUDIO_BENCH_CLIPS}/${clip}.pkt" "${out_dir}/${clip}.pkt" COPYONLY)
    list(APPEND clips "${out_dir}/${clip}.pkt")
endforeach()

devkit_rust_crate(ROOT "lib.rs"
    BINDGEN_HEADERS "audf_types.h" "audf_codec.h" "audf_aac.h" "audf_mp3.h" "audf_opus.h" "audf_flac.h"
                    "audf_alac.h" "audf_eq.h" "audf_gain.h"
    DEPENDS ${clips})
