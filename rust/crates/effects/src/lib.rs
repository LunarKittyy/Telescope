//! Transforms, Colour grading, Chroma key and the Fun pack, the plugins that change how the picture looks. Colour grading's maths (`grade`) and its
//! LUT files (`cube`) are here too; the pixel work goes through core's shader steps and colour pipeline.

pub mod auto_fit;
pub mod auto_grade;
pub mod auto_measure;
pub mod auto_tuning;
#[cfg(test)]
mod auto_scene;
#[cfg(test)]
mod auto_tests;
#[cfg(test)]
mod auto_grade_tests;
pub mod background;
pub mod chroma_key;
pub mod cinema;
pub mod cinema_paint;
pub mod cinema_tools;
pub mod composite;
pub mod composite_plugin;
pub mod compositor;
pub mod cpu;
pub mod cube;
pub mod cv_draw;
pub mod desk;
pub mod face_exposure;
pub mod framing;
pub mod fun_effects;
mod light_planes;
mod slit_planes;
pub mod fun_pack;
pub mod imgops;
pub mod mosh;
pub mod editors;
pub mod grade;
pub mod name_tag;
pub mod on_air;
pub mod on_air_plugin;
pub mod peek;
pub mod notes;
pub mod photos;
pub mod pixels;
pub mod privacy_blur;
pub mod qr_reader;
pub mod colour_grading;
pub mod shared;
pub mod scopes;
pub mod sheet;
pub mod tag;
pub mod transforms;
pub mod vision_row;
