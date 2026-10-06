# The app's authenticated control connection (ksip_ctrl.cpp). One of the
# objects of ksip_modules.lib (../CMakeLists.txt).
project(ksip_ctrl)
add_library(${PROJECT_NAME} OBJECT ksip_ctrl.cpp)
target_compile_features(${PROJECT_NAME} PRIVATE cxx_std_20)
target_compile_definitions(${PROJECT_NAME} PRIVATE NOMINMAX)
target_compile_options(${PROJECT_NAME} PRIVATE /utf-8)
