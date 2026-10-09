//! 外部プログラムの起動 (OS ごとの違いをここで吸収する)。

use std::process::Command;

/// 外部プログラムを起動するための Command を作る。
/// Windows では、GUI アプリから起動したときにコンソール画面が一瞬開かないようにする。
pub fn command(program: impl AsRef<std::ffi::OsStr>) -> Command {
    #[allow(unused_mut)]
    let mut cmd = Command::new(program);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    cmd
}
