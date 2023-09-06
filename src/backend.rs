use std::net::{SocketAddr, AddrParseError};
use std::str::FromStr;

pub trait GetBackend {
    fn get(&mut self) -> Option<SocketAddr>;
    fn add(&mut self, backend_str: &str) -> Result<(), AddrParseError>;
    fn remove(&mut self, backend_str: &str) -> Result<(), AddrParseError>;
}

pub struct RoundRobinBackend {
    backends: Vec<SocketAddr>,
    last_used: usize,
}

impl RoundRobinBackend {
    pub fn new(backends_str: Vec<String>) -> Result<RoundRobinBackend, AddrParseError> {
        let mut backends = Vec::new();
        for backend_str in backends_str {
            let backend_socket_addr: SocketAddr = FromStr::from_str(&backend_str)?;
            backends.push(backend_socket_addr);
            info!("Load balancing server {:?}", backend_socket_addr);
        }
        Ok(RoundRobinBackend {
            backends: backends,
            last_used: 0,
        })
    }
}

impl GetBackend for RoundRobinBackend {
    fn get(&mut self) -> Option<SocketAddr> {
        if self.backends.is_empty() {
            return None;
        }
        self.last_used = (self.last_used + 1) % self.backends.len();
        self.backends.get(self.last_used).map(|b| b.clone())
    }

    fn add(&mut self, backend_str: &str) -> Result<(), AddrParseError> {
        let backend_socket_addr: SocketAddr = FromStr::from_str(&backend_str)?;
        self.backends.push(backend_socket_addr);
        Ok(())
    }

    fn remove(&mut self, backend_str: &str) -> Result<(), AddrParseError> {
        let backend_socket_addr: SocketAddr = FromStr::from_str(&backend_str)?;
        self.backends.retain(|&x| x != backend_socket_addr);
        Ok(())
    }
}


#[cfg(test)]
mod tests {
    use std::net::{SocketAddr, AddrParseError};
    use super::{RoundRobinBackend, GetBackend};

    #[test]
    fn test_rrb_backend() {
        let backends_str = vec!["127.0.0.1:6000".to_string(), "127.0.0.1:6001".to_string()];
        let mut rrb = RoundRobinBackend::new(backends_str).unwrap();
        assert_eq!(2, rrb.backends.len());

        let first_socket_addr = rrb.get().unwrap();
        let second_socket_addr = rrb.get().unwrap();
        let third_socket_addr = rrb.get().unwrap();
        let fourth_socket_addr = rrb.get().unwrap();
        assert_eq!(first_socket_addr, third_socket_addr);
        assert_eq!(second_socket_addr, fourth_socket_addr);
        assert!(first_socket_addr != second_socket_addr);
    }

    #[test]
    fn test_empty_rrb_backend() {
        let backends_str = vec![];
        let mut rrb = RoundRobinBackend::new(backends_str).unwrap();
        assert_eq!(0, rrb.backends.len());
        assert!(rrb.get().is_none());
    }

    #[test]
    fn test_add_to_rrb_backend() {
        let mut rrb = RoundRobinBackend::new(vec![]).unwrap();
        assert!(rrb.get().is_none());
        assert!(rrb.add("327.0.0.1:6000").is_err());
        assert!(rrb.get().is_none());
        assert!(rrb.add("127.0.0.1:6000").is_ok());
        assert!(rrb.get().is_some());
    }

    #[test]
    fn test_remove_from_rrb_backend() {
        let backends_str = vec!["127.0.0.1:6000".to_string(), "127.0.0.1:6001".to_string()];
        let mut rrb = RoundRobinBackend::new(backends_str).unwrap();
        assert!(rrb.remove("327.0.0.1:6000").is_err());
        assert_eq!(2, rrb.backends.len());
        assert!(rrb.remove("127.0.0.1:6000").is_ok());
        assert_eq!(1, rrb.backends.len());
        assert!(rrb.remove("127.0.0.1:6000").is_ok());
        assert_eq!(1, rrb.backends.len());
    }

}
