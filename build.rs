fn build_configuration_schema() -> std::io::Result<()> {
    use configuration::Configuration;
    use schemars::{SchemaGenerator, generate::SchemaSettings};
    use std::{fs, path::Path};

    println!("cargo:rerun-if-changed=./configuration/src/*.rs");

    let path = Path::new("schemas");
    fs::create_dir_all(path)?;

    let mut generator07 = SchemaGenerator::new(SchemaSettings::draft07());

    let configuration_schema = generator07.root_schema_for::<Configuration>();
    let schema_file_path = path.join("config.schema.json");
    fs::write(
        schema_file_path,
        serde_json::to_string_pretty(&configuration_schema)?,
    )?;

    Ok(())
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    build_configuration_schema()?;
    Ok(())
}
