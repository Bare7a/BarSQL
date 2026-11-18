// The real binary's image header, as Windows reads it at launch.
#![cfg(all(windows, target_env = "msvc"))]

// GPUI draws on the main thread, and Windows gives that thread the stack this header asks for.
#[test]
fn the_main_thread_gets_8_mb_of_stack() {
    let exe = std::fs::read(env!("CARGO_BIN_EXE_BarSQL")).unwrap();
    let bytes = |at: usize, len: usize| &exe[at..at + len];
    let signature = u32::from_le_bytes(bytes(0x3c, 4).try_into().unwrap()) as usize;
    assert_eq!(bytes(signature, 4), b"PE\0\0");
    let optional_header = signature + 4 + 20;
    assert_eq!(u16::from_le_bytes(bytes(optional_header, 2).try_into().unwrap()), 0x20b, "a PE32+ image");
    let stack_reserve = u64::from_le_bytes(bytes(optional_header + 72, 8).try_into().unwrap());
    assert_eq!(stack_reserve, 8 << 20);
}
