# GitHub Configuration

This directory contains GitHub-specific configuration files for Redistill.

## Workflows

### CI Workflow (`workflows/ci.yml`)
Runs on every push and pull request to `main` or `develop` branches.

**Checks:**
- ✅ Unit tests on Linux, macOS, Windows
- ✅ Code formatting (`cargo fmt`)
- ✅ Linting (`cargo clippy`)
- ✅ Security audit (`cargo audit`)
- ✅ Build verification

**Badge:**
```markdown
![CI](https://github.com/YOUR_USERNAME/redistill/workflows/CI/badge.svg)
```

### Release Workflow (`workflows/release.yml`)
Runs when a version tag (e.g., `v1.0.0`) is pushed.

**Actions:**
- 🏗️ Builds binaries for all supported platforms
- 📦 Creates GitHub release automatically
- ⬆️ Uploads binaries as release assets
- 🔐 Generates SHA256 checksums

**Supported Platforms:**
- Linux x86_64 (glibc)
- Linux x86_64 (musl - static)
- Linux ARM64
- macOS x86_64 (Intel)
- macOS ARM64 (Apple Silicon)
- Windows x86_64

**Badge:**
```markdown
![Release](https://github.com/YOUR_USERNAME/redistill/workflows/Release/badge.svg)
```

## Usage

### Running Tests Locally
```bash
# All tests
cargo test --release

# Specific test
cargo test test_name

# With output
cargo test -- --nocapture
```

### Code Quality Checks
```bash
# Format code
cargo fmt

# Run linter
cargo clippy --all-targets --all-features

# Security audit
cargo audit
```

### Creating a Release

See [RELEASE_PROCESS.md](RELEASE_PROCESS.md) for detailed instructions.

**Quick steps:**
```bash
# Update version in Cargo.toml
# Update CHANGELOG.md
git add Cargo.toml docs/CHANGELOG.md
git commit -m "Bump version to 1.1.0"
git push origin main

# Create and push tag
git tag v1.1.0
git push origin v1.1.0
```

GitHub Actions will automatically:
1. Build binaries for all platforms
2. Create a GitHub release
3. Upload artifacts

## Files

```
.github/
├── workflows/
│   ├── ci.yml           # Continuous Integration
│   └── release.yml      # Release builds
├── RELEASE_PROCESS.md   # Detailed release guide
└── README.md            # This file
```

## Workflow Status

Check workflow runs: https://github.com/YOUR_USERNAME/redistill/actions

### Recent Runs
- **CI**: Tests and quality checks
- **Release**: Binary builds and releases

## Configuration

### Secrets Required
No secrets are required for public repositories. For private repositories:
- `GITHUB_TOKEN` (automatically provided by GitHub)

### Caching
Workflows use caching to speed up builds:
- Cargo registry
- Cargo index
- Build artifacts

Cache is automatically managed by GitHub Actions.

## Troubleshooting

### CI Failing
1. Check test output in Actions tab
2. Run tests locally: `cargo test --release`
3. Fix issues and push

### Release Failing
1. Check Actions logs for specific platform
2. Verify Cargo.toml syntax
3. Ensure tag follows `v*` pattern

### Build Time
- **CI**: ~5-10 minutes
- **Release**: ~10-15 minutes (all platforms)

## Best Practices

1. **Always run tests locally** before pushing
2. **Update CHANGELOG.md** with every release
3. **Use semantic versioning** (MAJOR.MINOR.PATCH)
4. **Test on multiple platforms** if making low-level changes
5. **Review Actions logs** if builds fail

## Resources

- [GitHub Actions Documentation](https://docs.github.com/en/actions)
- [Cargo Documentation](https://doc.rust-lang.org/cargo/)
- [Semantic Versioning](https://semver.org/)

---

For questions about CI/CD, open an issue on GitHub.

