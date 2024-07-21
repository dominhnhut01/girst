mod data_model;
mod girst_init;
use std::{env, path::PathBuf};

use data_model::{Blob, Database, Workspace};

fn main() {
    let args: Vec<String> = env::args().collect();
    let command = (&args[1]).as_str();

    match command {
        "init" => match args.get(2) {
            Some(working_dir) => match girst_init::girst_init(working_dir) {
                Ok(()) => println!("Successfully initialized .girst"),
                Err(e) => panic!("Error initializing .girst: {}", e),
            },
            None => panic!("Error: No argument provided for init."),
        },
        "commit" => match args.get(2) {
            Some(root_dir) => {
                let girst_path = PathBuf::from(root_dir).join(".girst");
                let db_path = girst_path.join("objects");

                let workspace = Workspace::new(root_dir);
                let database = Database::new(db_path);

                for file_path in workspace.list_files() {
                    let data = workspace.read_file(file_path);
                    let blob = Blob::new(data);

                    
                }
            }
            None => panic!("Error: No root dir  ectory provided."),
        },
        _ => print!("{} is not a valid command.", command),
    }
}
