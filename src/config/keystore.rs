use aes_gcm::{
    aead::{Aead, KeyInit},
    Aes256Gcm, Nonce,
};
use argon2::Argon2;
use base64::{engine::general_purpose::STANDARD as BASE64, Engine};
use rand::RngCore;
use std::path::Path;
use crate::{Error, Result};

/// Key store for encrypting/decrypting API keys
pub struct KeyStore {
    cipher: Aes256Gcm,
}

impl KeyStore {
    /// Create a new KeyStore with a key derived from machine UUID + app salt.
    /// salt 存于 data_dir——迁移（migrate_legacy_config）必须先于本调用，
    /// 否则新目录会先生成新盐，旧密文永不可解
    pub fn new(data_dir: &Path) -> Result<Self> {
        let machine_id = Self::get_machine_id()?;
        let salt = Self::get_or_create_salt(data_dir)?;
        
        // Derive key using Argon2
        let mut key = [0u8; 32];
        Argon2::default()
            .hash_password_into(&machine_id, &salt, &mut key)
            .map_err(|e| Error::Config(format!("Key derivation failed: {}", e)))?;
        
        let cipher = Aes256Gcm::new(&key.into());
        Ok(Self { cipher })
    }
    
    /// Encrypt a plaintext string
    pub fn encrypt(&self, plaintext: &str) -> Result<String> {
        let mut nonce_bytes = [0u8; 12];
        rand::thread_rng().fill_bytes(&mut nonce_bytes);
        let nonce = Nonce::from_slice(&nonce_bytes);
        
        let ciphertext = self.cipher
            .encrypt(nonce, plaintext.as_bytes())
            .map_err(|e| Error::Config(format!("Encryption failed: {}", e)))?;
        
        // Combine nonce + ciphertext and encode as base64
        let mut combined = nonce_bytes.to_vec();
        combined.extend_from_slice(&ciphertext);
        Ok(BASE64.encode(combined))
    }
    
    /// Decrypt a base64-encoded ciphertext
    pub fn decrypt(&self, encoded: &str) -> Result<String> {
        let combined = BASE64.decode(encoded)
            .map_err(|e| Error::Config(format!("Base64 decode failed: {}", e)))?;
        
        if combined.len() < 12 {
            return Err(Error::Config("Invalid ciphertext length".to_string()));
        }
        
        let (nonce_bytes, ciphertext) = combined.split_at(12);
        let nonce = Nonce::from_slice(nonce_bytes);
        
        let plaintext = self.cipher
            .decrypt(nonce, ciphertext)
            .map_err(|e| Error::Config(format!("Decryption failed: {}", e)))?;
        
        String::from_utf8(plaintext)
            .map_err(|e| Error::Config(format!("UTF-8 decode failed: {}", e)))
    }
    
    /// Get machine unique identifier (macOS: IOPlatformUUID)
    #[cfg(target_os = "macos")]
    fn get_machine_id() -> Result<Vec<u8>> {
        use std::process::Command;
        
        let output = Command::new("ioreg")
            .args(&["-d2", "-c", "IOPlatformExpertDevice"])
            .output()
            .map_err(|e| Error::Config(format!("Failed to get machine ID: {}", e)))?;
        
        let stdout = String::from_utf8_lossy(&output.stdout);
        let uuid = stdout
            .lines()
            .find(|line| line.contains("IOPlatformUUID"))
            .and_then(|line| line.split('"').nth(3))
            .ok_or_else(|| Error::Config("IOPlatformUUID not found".to_string()))?;
        
        Ok(uuid.as_bytes().to_vec())
    }
    
    /// Windows：注册表 MachineGuid（HKLM\SOFTWARE\Microsoft\Cryptography）
    #[cfg(target_os = "windows")]
    fn get_machine_id() -> Result<Vec<u8>> {
        use windows::core::HSTRING;
        use windows::Win32::System::Registry::{
            RegGetValueW, HKEY_LOCAL_MACHINE, RRF_RT_REG_SZ,
        };
        let subkey = HSTRING::from("SOFTWARE\\Microsoft\\Cryptography");
        let value = HSTRING::from("MachineGuid");
        let mut buf = [0u16; 128];
        let mut len = (buf.len() * 2) as u32;
        let ret = unsafe {
            RegGetValueW(
                HKEY_LOCAL_MACHINE,
                &subkey,
                &value,
                RRF_RT_REG_SZ,
                None,
                Some(buf.as_mut_ptr().cast()),
                Some(&mut len),
            )
        };
        if ret.is_err() {
            return Err(Error::Config(format!("读取 MachineGuid 失败: {ret:?}")));
        }
        let s = String::from_utf16_lossy(&buf[..(len as usize / 2).saturating_sub(1)]);
        Ok(s.trim().as_bytes().to_vec())
    }

    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    fn get_machine_id() -> Result<Vec<u8>> {
        // Fallback for other platforms
        Ok(b"default-machine-id".to_vec())
    }

    /// Get or create app salt
    fn get_or_create_salt(data_dir: &Path) -> Result<Vec<u8>> {
        let salt_path = data_dir.join("salt");

        if salt_path.exists() {
            let salt = std::fs::read(&salt_path)?;
            Ok(salt)
        } else {
            let mut salt = vec![0u8; 32];
            rand::thread_rng().fill_bytes(&mut salt);

            if let Some(parent) = salt_path.parent() {
                std::fs::create_dir_all(parent)?;
            }
            std::fs::write(&salt_path, &salt)?;

            // Set permissions to 0600 (owner read/write only)
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let mut perms = std::fs::metadata(&salt_path)?.permissions();
                perms.set_mode(0o600);
                std::fs::set_permissions(&salt_path, perms)?;
            }

            Ok(salt)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_encrypt_decrypt() {
        let dir = tempfile::tempdir().unwrap();
        let keystore = KeyStore::new(dir.path()).unwrap();
        let plaintext = "test-api-key-12345";

        let encrypted = keystore.encrypt(plaintext).unwrap();
        let decrypted = keystore.decrypt(&encrypted).unwrap();

        assert_eq!(plaintext, decrypted);
    }
}
