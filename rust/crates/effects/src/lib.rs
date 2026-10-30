//! Transforms, Colour grading, Chroma key and the Fun pack, the plugins that change how the picture looks. Colour grading's maths (`grade`) and its
//! LUT files (`cube`) are here too; the pixel work goes through core's shader steps and colour pipeline.

pub mod chroma_key;
pub mod composite;
pub mod composite_plugin;
pub mod compositor;
pub mod cpu;
pub mod cube;
pub mod desk;
pub mod fun_effects;
pub mod fun_pack;
pub mod imgops;
pub mod mosh;
pub mod editors;
pub mod grade;
pub mod name_tag;
pub mod on_air;
pub mod on_air_plugin;
pub mod notes;
pub mod photos;
pub mod pixels;
pub mod qr_reader;
pub mod colour_grading;
pub mod shared;
pub mod sheet;
pub mod tag;
pub mod transforms;
