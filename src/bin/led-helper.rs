// Setuid-root helper for SMC writes.
// Protocol (line-based stdin/stdout):
//   PING                     -> PONG
//   WRITE <key> <hex bytes>  -> OK / ERR <msg>

#[link(name = "IOKit", kind = "framework")]
extern "C" {}

use std::io::{self, BufRead, Write};

mod smc {
    use std::io;
    use std::mem;

    type IOReturn = i32;
    type MachPort = u32;
    type IOConnect = u32;
    type IOService = u32;
    const KERN_SUCCESS: IOReturn = 0;
    const KERNEL_INDEX_SMC: u32 = 2;

    extern "C" {
        fn mach_task_self() -> MachPort;
        fn IOServiceMatching(name: *const u8) -> *mut std::ffi::c_void;
        fn IOServiceGetMatchingService(
            master_port: MachPort,
            matching: *mut std::ffi::c_void,
        ) -> IOService;
        fn IOServiceOpen(
            service: IOService,
            owning_task: MachPort,
            connect_type: u32,
            connection: *mut IOConnect,
        ) -> IOReturn;
        fn IOServiceClose(connection: IOConnect) -> IOReturn;
        fn IOConnectCallStructMethod(
            connection: IOConnect,
            selector: u32,
            input: *const u8,
            input_cnt: usize,
            output: *mut u8,
            output_cnt: *mut usize,
        ) -> IOReturn;
        fn IOObjectRelease(object: u32) -> IOReturn;
    }

    #[repr(C)]
    struct SMCKeyInfoData {
        data_size: u32,
        data_type: u32,
        data_attributes: u8,
    }

    #[repr(C)]
    struct SMCKeyData {
        key: u32,
        vers: [u8; 6],
        p_limit_data: [u8; 16],
        key_info: SMCKeyInfoData,
        result: u8,
        status: u8,
        data8: u8,
        data32: u32,
        bytes: [u8; 32],
    }

    impl SMCKeyData {
        fn new() -> Self {
            unsafe { mem::zeroed() }
        }
    }

    pub struct SmcConn(IOConnect);

    impl SmcConn {
        pub fn open() -> io::Result<Self> {
            unsafe {
                let matching = IOServiceMatching(b"AppleSMC\0".as_ptr());
                if matching.is_null() {
                    return Err(io::Error::new(io::ErrorKind::NotFound, "no AppleSMC"));
                }
                let service = IOServiceGetMatchingService(0, matching);
                if service == 0 {
                    return Err(io::Error::new(io::ErrorKind::NotFound, "no service"));
                }
                let mut conn: IOConnect = 0;
                let r = IOServiceOpen(service, mach_task_self(), 0, &mut conn);
                IOObjectRelease(service);
                if r != KERN_SUCCESS {
                    return Err(io::Error::new(
                        io::ErrorKind::PermissionDenied,
                        format!("open failed: {}", r),
                    ));
                }
                Ok(Self(conn))
            }
        }

        fn call(&self, input: &SMCKeyData) -> io::Result<SMCKeyData> {
            unsafe {
                let mut output = SMCKeyData::new();
                let mut out_size = mem::size_of::<SMCKeyData>();
                let r = IOConnectCallStructMethod(
                    self.0,
                    KERNEL_INDEX_SMC,
                    input as *const _ as *const u8,
                    mem::size_of::<SMCKeyData>(),
                    &mut output as *mut _ as *mut u8,
                    &mut out_size,
                );
                if r != KERN_SUCCESS {
                    return Err(io::Error::new(
                        io::ErrorKind::Other,
                        format!("call failed: {}", r),
                    ));
                }
                Ok(output)
            }
        }

        pub fn write_key(&self, key: &str, data: &[u8]) -> io::Result<()> {
            let mut k = [0u8; 4];
            for (i, &b) in key.as_bytes().iter().take(4).enumerate() {
                k[i] = b;
            }
            let key_u32 = u32::from_be_bytes(k);

            // First call: get key info (data_size)
            let mut input = SMCKeyData::new();
            input.key = key_u32;
            input.data8 = 9; // kSMCGetKeyInfo
            let info = self.call(&input)?;

            // Second call: write
            let mut input = SMCKeyData::new();
            input.key = key_u32;
            input.data8 = 6; // kSMCWriteKey
            input.key_info.data_size = info.key_info.data_size;
            let len = data.len().min(32);
            input.bytes[..len].copy_from_slice(&data[..len]);
            self.call(&input)?;
            Ok(())
        }
    }

    impl Drop for SmcConn {
        fn drop(&mut self) {
            unsafe {
                IOServiceClose(self.0);
            }
        }
    }
}

fn main() {
    let conn = match smc::SmcConn::open() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("led-helper: failed to open SMC: {}", e);
            std::process::exit(1);
        }
    };

    let stdin = io::stdin();
    let mut stdout = io::stdout();

    for line in stdin.lock().lines() {
        let line = match line {
            Ok(l) => l,
            Err(_) => break,
        };
        let parts: Vec<&str> = line.trim().splitn(3, ' ').collect();
        match parts.as_slice() {
            ["PING"] => {
                let _ = writeln!(stdout, "PONG");
            }
            ["WRITE", key, hex_data] => {
                let bytes: Result<Vec<u8>, _> = hex_data
                    .split_whitespace()
                    .map(|s| u8::from_str_radix(s, 16))
                    .collect();
                match bytes {
                    Ok(data) => match conn.write_key(key, &data) {
                        Ok(()) => {
                            let _ = writeln!(stdout, "OK");
                        }
                        Err(e) => {
                            let _ = writeln!(stdout, "ERR {}", e);
                        }
                    },
                    Err(e) => {
                        let _ = writeln!(stdout, "ERR bad hex: {}", e);
                    }
                }
            }
            _ => {
                let _ = writeln!(stdout, "ERR unknown command");
            }
        }
        let _ = stdout.flush();
    }
}
