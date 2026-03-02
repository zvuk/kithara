use std::{collections::HashMap, path::PathBuf};

fn main() {
    let lucide_path = lucide_slint::get_slint_file_path();
    let library = HashMap::from([("lucide".to_string(), PathBuf::from(lucide_path))]);

    let config = slint_build::CompilerConfiguration::new().with_library_paths(library);
    slint_build::compile_with_config("ui/app.slint", config).expect("Slint compilation failed");
    slint_build::print_rustc_flags().expect("Failed to print Slint rustc flags");
}
