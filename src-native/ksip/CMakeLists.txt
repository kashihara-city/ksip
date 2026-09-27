project(ksip)
list(APPEND MODULES_DETECTED ${PROJECT_NAME})
set(MODULES_DETECTED ${MODULES_DETECTED} PARENT_SCOPE)
add_library(${PROJECT_NAME} OBJECT ksip.cpp ksip_io.cpp ksip_text.cpp sip_account.cpp calls.cpp transfer.cpp subscriptions.cpp trust_state.cpp)
target_compile_features(${PROJECT_NAME} PRIVATE cxx_std_17)
target_compile_definitions(${PROJECT_NAME} PRIVATE NOMINMAX)
target_compile_options(${PROJECT_NAME} PRIVATE /utf-8)
# The audio module's state header (audio_state.h) sits beside this module; the
# trust store is read through LibreSSL, which the engine links anyway.
target_include_directories(${PROJECT_NAME} PRIVATE "${KSIP_NATIVE}/include/ksip" "${CMAKE_CURRENT_SOURCE_DIR}/../ksip_audio" "${OPENSSL_INCLUDE_DIR}")
target_link_libraries(${PROJECT_NAME} PRIVATE advapi32)
