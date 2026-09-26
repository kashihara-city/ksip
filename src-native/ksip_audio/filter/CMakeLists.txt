# The filter module (recording and software gain) is built from the sources
# beside the device module: one folder of audio code, two registrations with
# baresip. patch-baresip.py puts this file where baresip looks for the module.
project(postlab)
list(APPEND MODULES_DETECTED ${PROJECT_NAME})
set(MODULES_DETECTED ${MODULES_DETECTED} PARENT_SCOPE)
add_library(${PROJECT_NAME} OBJECT
  ../ksip_audio/filter_module.cpp
  ../ksip_audio/recording_session.cpp
  ../ksip_audio/recorder.cpp)
target_compile_features(${PROJECT_NAME} PRIVATE cxx_std_17)
target_compile_definitions(${PROJECT_NAME} PRIVATE NOMINMAX)
target_compile_options(${PROJECT_NAME} PRIVATE /utf-8)
target_include_directories(${PROJECT_NAME} PRIVATE ../ksip_audio)
target_link_libraries(${PROJECT_NAME} PRIVATE ole32 uuid)
