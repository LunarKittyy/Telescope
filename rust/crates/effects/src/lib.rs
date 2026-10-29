//! Transforms and Colour grading, the two plugins that change how the picture looks. Colour grading's maths (`grade`) and its
//! LUT files (`cube`) are here too; the pixel work goes through core's shader steps and colour pipeline.

pub mod cube;
pub mod desk;
pub mod editors;
pub mod grade;
pub mod name_tag;
pub mod on_air;
pub mod on_air_plugin;
pub mod notes;
pub mod pixels;
pub mod qr_reader;
pub mod colour_grading;
pub mod shared;
pub mod sheet;
pub mod tag;
pub mod transforms;
