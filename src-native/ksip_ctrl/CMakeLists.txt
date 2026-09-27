# The app's authenticated control connection (ksip_ctrl.cpp). patch-baresip.py
# copies this folder to modules/ksip_ctrl/, where baresip looks for a module.
project(ksip_ctrl)
list(APPEND MODULES_DETECTED ${PROJECT_NAME})
set(MODULES_DETECTED ${MODULES_DETECTED} PARENT_SCOPE)
add_library(${PROJECT_NAME} OBJECT ksip_ctrl.cpp)
target_compile_features(${PROJECT_NAME} PRIVATE cxx_std_17)
target_compile_definitions(${PROJECT_NAME} PRIVATE NOMINMAX)
target_compile_options(${PROJECT_NAME} PRIVATE /utf-8)
