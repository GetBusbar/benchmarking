use super::RealLauncher;

impl RealLauncher {
    pub(crate) fn native_pid(&self) -> Option<u32> {
        self.native_child.as_ref().map(std::process::Child::id)
    }
}
