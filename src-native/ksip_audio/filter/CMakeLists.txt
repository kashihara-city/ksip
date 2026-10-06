# The filter module (recording and software gain) is built from the sources
# beside the device module: one folder of audio code, two registrations with
# baresip. This folder holds only the module's build file. One of the objects
# of ksip_modules.lib (../../CMakeLists.txt).
project(ksip_audio_filter)
add_library(${PROJECT_NAME} OBJECT
  ../filter_module.cpp
  ../recording_session.cpp
  ../recorder.cpp
  ../inband_dtmf.cpp)
target_compile_features(${PROJECT_NAME} PRIVATE cxx_std_20)
target_compile_definitions(${PROJECT_NAME} PRIVATE NOMINMAX)
target_compile_options(${PROJECT_NAME} PRIVATE /utf-8)
target_include_directories(${PROJECT_NAME} PRIVATE ..)
target_link_libraries(${PROJECT_NAME} PRIVATE ole32 uuid)
