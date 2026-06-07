// ta-init — Template engine and feature components for `ta init` (v0.16.5).
//
// Templates are defined as TOML manifests (bundled or user-installed) and
// compose named `Feature` trait objects. This allows community templates
// without code changes to TA itself.

pub mod bundled;
pub mod engine;
pub mod feature;
pub mod scaffold;

pub use engine::{parse_manifest, InitTemplate, TemplateManifest};
pub use feature::{Feature, TemplateContext};
