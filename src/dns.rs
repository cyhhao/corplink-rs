use std::collections::HashMap;
use std::fs;
use std::io::{Error, ErrorKind};
use std::path::Path;
use std::process::Command;

const RESOLVER_DIR: &str = "/etc/resolver";

pub struct DNSManager {
    // For /etc/resolver method
    created_resolver_files: Vec<String>,
    // For networksetup method (fallback)
    service_dns: HashMap<String, String>,
    service_dns_search: HashMap<String, String>,
}

impl DNSManager {
    pub fn new() -> DNSManager {
        DNSManager {
            created_resolver_files: Vec::new(),
            service_dns: HashMap::new(),
            service_dns_search: HashMap::new(),
        }
    }

    fn collect_new_service_dns(&mut self) -> Result<(), Error> {
        let output = Command::new("networksetup")
            .arg("-listallnetworkservices")
            .output()?;

        let services = String::from_utf8_lossy(&output.stdout);
        let lines = services.lines();
        for service in lines.skip(1) {
            let service = service.trim_start_matches('*').trim();
            if service.is_empty() {
                continue;
            }

            let dns_output = Command::new("networksetup")
                .arg("-getdnsservers")
                .arg(service)
                .output()?;
            let dns_response = String::from_utf8_lossy(&dns_output.stdout)
                .trim()
                .to_string();
            let dns_response = if dns_response.contains(" ") {
                "Empty".to_string()
            } else {
                dns_response
            };

            self.service_dns
                .insert(service.to_string(), dns_response.clone());

            let search_output = Command::new("networksetup")
                .arg("-getsearchdomains")
                .arg(service)
                .output()?;
            let search_response = String::from_utf8_lossy(&search_output.stdout)
                .trim()
                .to_string();
            let search_response = if search_response.contains(" ") {
                "Empty".to_string()
            } else {
                search_response
            };

            self.service_dns_search
                .insert(service.to_string(), search_response.clone());

            log::debug!(
                "DNS collected for {}, dns servers: {}, search domain: {}",
                service,
                dns_response,
                search_response
            )
        }
        Ok(())
    }

    /// Set DNS using networksetup (system-wide DNS change)
    fn set_dns_networksetup(&mut self, dns_servers: Vec<&str>, dns_search: Vec<&str>) -> Result<(), Error> {
        self.collect_new_service_dns()?;
        
        for service in self.service_dns.keys() {
            Command::new("networksetup")
                .arg("-setdnsservers")
                .arg(service)
                .args(&dns_servers)
                .status()?;

            if !dns_search.is_empty() {
                Command::new("networksetup")
                    .arg("-setsearchdomains")
                    .arg(service)
                    .args(&dns_search)
                    .status()?;
            }
            log::debug!("DNS set for {} with {}", service, dns_servers.join(","));
        }
        Ok(())
    }

    fn restore_dns_networksetup(&self) -> Result<(), Error> {
        for (service, dns) in &self.service_dns {
            Command::new("networksetup")
                .arg("-setdnsservers")
                .arg(service)
                .args(dns.lines())
                .status()?;
            log::debug!("DNS server reset for {} with {}", service, dns);
        }
        for (service, search_domain) in &self.service_dns_search {
            Command::new("networksetup")
                .arg("-setsearchdomains")
                .arg(service)
                .args(search_domain.lines())
                .status()?;
            log::debug!("DNS search domain reset for {} with {}", service, search_domain)
        }
        Ok(())
    }

    /// Create resolver file for a specific domain
    fn create_resolver_file(&mut self, domain: &str, dns_servers: &[&str]) -> Result<(), Error> {
        // Ensure /etc/resolver directory exists
        if !Path::new(RESOLVER_DIR).exists() {
            fs::create_dir_all(RESOLVER_DIR).map_err(|e| {
                Error::new(ErrorKind::Other, format!("failed to create {}: {}", RESOLVER_DIR, e))
            })?;
        }

        let resolver_file = format!("{}/{}", RESOLVER_DIR, domain);
        
        let mut content = String::new();
        for server in dns_servers {
            content.push_str(&format!("nameserver {}\n", server));
        }
        content.push_str("timeout 5\n");
        
        fs::write(&resolver_file, &content).map_err(|e| {
            Error::new(ErrorKind::Other, format!("failed to write {}: {}", resolver_file, e))
        })?;
        
        self.created_resolver_files.push(resolver_file.clone());
        log::info!("Created resolver: {} -> {}", resolver_file, dns_servers.join(", "));
        Ok(())
    }

    /// Parse domain from pattern like "*.*" or "*.example.com" or "example.com"
    fn extract_domains_from_patterns(patterns: &[String]) -> Vec<String> {
        let mut domains = Vec::new();
        
        for pattern in patterns {
            // "*.*" means all domains - we'll use networksetup method instead
            if pattern == "*.*" || pattern == "*" {
                continue;
            }
            
            // "*.example.com" -> "example.com"
            let domain = pattern.trim_start_matches("*.");
            if !domain.is_empty() && domain.contains('.') {
                domains.push(domain.to_string());
            }
        }
        
        domains
    }

    pub fn set_dns(&mut self, dns_servers: Vec<&str>, dns_domain_split: Vec<&str>) -> Result<(), Error> {
        if dns_servers.is_empty() {
            return Ok(());
        }

        let patterns: Vec<String> = dns_domain_split.iter().map(|s| s.to_string()).collect();
        let domains = Self::extract_domains_from_patterns(&patterns);
        
        // Check if it's a catch-all pattern (*.*) or no specific domains
        let is_catch_all = dns_domain_split.is_empty() 
            || dns_domain_split.iter().any(|p| *p == "*.*" || *p == "*");

        if is_catch_all {
            // Use networksetup for system-wide DNS
            log::info!("Setting system-wide DNS: {}", dns_servers.join(", "));
            return self.set_dns_networksetup(dns_servers, vec![]);
        }

        // Create resolver files for specific domains
        for domain in &domains {
            self.create_resolver_file(domain, &dns_servers)?;
        }

        Ok(())
    }

    pub fn restore_dns(&self) -> Result<(), Error> {
        // Remove created resolver files
        for file in &self.created_resolver_files {
            if Path::new(file).exists() {
                fs::remove_file(file)?;
                log::debug!("Removed resolver file: {}", file);
            }
        }
        
        // Restore networksetup DNS if it was used
        if !self.service_dns.is_empty() {
            self.restore_dns_networksetup()?;
        }
        
        log::debug!("DNS restored");
        Ok(())
    }
}
