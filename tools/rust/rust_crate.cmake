# cmake -P step behind devkit_rust_crate(): runs rustc, merges its codegen-unit
# objects into OBJ, and rewrites rustc's dep-info into a depfile for OBJ alone.

file(GLOB stale "${CGU_DIR}/*.o")
if(stale)
    file(REMOVE ${stale})
endif()

set(argfiles "")
foreach(f ${ARGS})
    list(APPEND argfiles "@${f}")
endforeach()
execute_process(COMMAND "${RUSTC}" ${argfiles} RESULT_VARIABLE result)
if(NOT result EQUAL 0)
    message(FATAL_ERROR "rustc failed")
endif()

file(GLOB objs "${CGU_DIR}/*.o")
list(LENGTH objs count)
if(count EQUAL 0)
    message(FATAL_ERROR "rustc produced no object in ${CGU_DIR}")
elseif(count EQUAL 1)
    file(COPY_FILE "${objs}" "${OBJ}")
else()
    execute_process(COMMAND "${LINKER}" -r -o "${OBJ}" ${objs} RESULT_VARIABLE result)
    if(NOT result EQUAL 0)
        message(FATAL_ERROR "${LINKER} -r failed for ${OBJ}")
    endif()
endif()

# rustc names every emitted file as a target; ninja wants only this edge's output.
file(STRINGS "${RUSTC_DEPFILE}" lines)
list(GET lines 0 first)
string(FIND "${first}" ": " colon)
math(EXPR start "${colon} + 2")
string(SUBSTRING "${first}" ${start} -1 inputs)
file(WRITE "${DEPFILE}" "${OBJ}: ${inputs}\n")
