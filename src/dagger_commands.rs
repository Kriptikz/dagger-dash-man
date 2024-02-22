use std::path::Path;

use tokio::process::Command;

pub struct DaggerCommands {
    pub path_to_wield_binary: String,
    pub path_to_new_wield_binary: String,
    pub wield_binary_url: String,
}

impl DaggerCommands {

    pub async fn stop_node(&self) -> Result<(), ()> {
        let result = Command::new("systemctl")
            .arg("stop")
            .arg("wield")
            .output()
            .await;

        if let Ok(_) = result {
            return Ok(())
        }
    
        Err(())
    }

    pub async fn start_node(&self) -> Result<(), ()> {
        let result = Command::new("systemctl")
            .arg("start")
            .arg("wield")
            .output()
            .await;

        if let Ok(_) = result {
            return Ok(())
        }
    
        Err(())
    }

    pub async fn get_current_version(&self) -> Result<String, ()> {
        if Path::new(&self.path_to_wield_binary).exists() {
            let output = Command::new(self.path_to_wield_binary.clone())
                .arg("--version")
                .output()
                .await
                .expect(&format!(
                    "Should successfully run wield --version from path {}",
                    self.path_to_wield_binary
                ));

            let current_binary_version = String::from_utf8(output.stdout);
            if let Ok(current_binary_version) = current_binary_version {
                return Ok(current_binary_version);
            }
        }

        Err(())
    }

    pub async fn get_new_binary_version(&self) -> Result<String, ()> {
        if Path::new(&self.path_to_new_wield_binary).exists() {
            let output = Command::new(self.path_to_wield_binary.clone())
                .arg("--version")
                .output()
                .await
                .expect(&format!(
                    "Should successfully run wield --version from path {}",
                    self.path_to_wield_binary
                ));

            let current_binary_version = String::from_utf8(output.stdout);

            if let Ok(current_binary_version) = current_binary_version {
                return Ok(current_binary_version);
            }
        }
        Err(())
    }
}
