//! EIP-170 contract size limit enforcement for snark-verifier Yul output.

/// Maximum deployable runtime bytecode size per EIP-170.
pub const EIP170_MAX_BYTES: usize = 24_576;

/// Assert `bytecode.len() <= EIP170_MAX_BYTES`, returning a descriptive error
/// on violation.
pub fn assert_eip170(bytecode: &[u8], label: &str) -> anyhow::Result<usize> {
    let size = bytecode.len();
    if size > EIP170_MAX_BYTES {
        anyhow::bail!(
            "{label}: verifier bytecode {size} bytes exceeds EIP-170 limit {EIP170_MAX_BYTES}"
        );
    }
    Ok(size)
}

/// Scan every `.bin` under `dir` and fail if any exceeds the limit.
pub fn check_bin_dir(dir: &std::path::Path) -> anyhow::Result<Vec<(String, usize)>> {
    let mut sizes = Vec::new();
    if !dir.is_dir() {
        return Ok(sizes);
    }
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("bin") {
            continue;
        }
        let bytes = std::fs::read(&path)?;
        let label = path.display().to_string();
        let size = assert_eip170(&bytes, &label)?;
        sizes.push((label, size));
    }
    Ok(sizes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn under_limit_passes() {
        assert!(assert_eip170(&[0u8; 100], "test").is_ok());
    }

    #[test]
    fn over_limit_fails() {
        assert!(assert_eip170(&vec![0u8; EIP170_MAX_BYTES + 1], "test").is_err());
    }
}
