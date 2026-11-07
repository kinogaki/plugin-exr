//! OpenEXR as a Studio plugin library (`formats/exr.dylib`).
use kinogaki_studio_api::export_package;

kinogaki_shared::link!();
export_package!(kinogaki_exr_format::package);
