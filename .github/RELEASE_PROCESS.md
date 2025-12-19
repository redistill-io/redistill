# Release Process

This document describes how to create releases for Redistill using GitHub Actions.

## Overview

Redistill uses GitHub Actions for automated building and releasing across multiple platforms:
- **Linux**: x86_64 (glibc), x86_64 (musl/static), ARM64
- **macOS**: x86_64 (Intel), ARM64 (Apple Silicon)
- **Windows**: x86_64

## Automated Release Workflow

### Prerequisites

1. Ensure all changes are committed and pushed to `main`
2. All tests are passing (CI workflow is green)
3. Update version in `Cargo.toml`
4. Update `docs/CHANGELOG.md` with release notes

### Creating a Release

#### Step 1: Create a Git Tag

```bash
# Update version in Cargo.toml first
vim Cargo.toml  # Change version = "1.0.0" to "1.1.0"

# Commit the version change
git add Cargo.toml
git commit -m "Bump version to 1.1.0"
git push origin main

# Create and push the tag
git tag v1.1.0
git push origin v1.1.0
```

#### Step 2: Automated Build Process

Once you push a tag starting with `v`, GitHub Actions will automatically:

1. **Create a GitHub Release** with version info
2. **Build binaries** for all supported platforms:
   - `redistill-1.1.0-x86_64-unknown-linux-gnu.tar.gz` (Linux glibc)
   - `redistill-1.1.0-x86_64-unknown-linux-musl.tar.gz` (Linux static)
   - `redistill-1.1.0-aarch64-unknown-linux-gnu.tar.gz` (Linux ARM64)
   - `redistill-1.1.0-x86_64-apple-darwin.tar.gz` (macOS Intel)
   - `redistill-1.1.0-aarch64-apple-darwin.tar.gz` (macOS Apple Silicon)
   - `redistill-1.1.0-x86_64-pc-windows-msvc.zip` (Windows)
3. **Strip binaries** to reduce size
4. **Upload artifacts** to the GitHub release
5. **Generate checksums** for verification

#### Step 3: Edit Release Notes (Optional)

After the automated release is created:

1. Go to: https://github.com/YOUR_USERNAME/redistill/releases
2. Click "Edit" on the new release
3. Enhance the auto-generated release notes with:
   - Key features/changes
   - Breaking changes (if any)
   - Bug fixes
   - Performance improvements
   - Links to relevant issues/PRs

## Release Checklist

Before creating a release tag:

- [ ] All tests passing locally (`cargo test --release`)
- [ ] Version updated in `Cargo.toml`
- [ ] `docs/CHANGELOG.md` updated with changes
- [ ] Breaking changes documented (if any)
- [ ] Performance benchmarks run (if significant changes)
- [ ] Documentation updated for new features
- [ ] CI passing on GitHub

## Version Numbering

Redistill follows [Semantic Versioning](https://semver.org/):

- **MAJOR** (1.x.x): Breaking changes
- **MINOR** (x.1.x): New features, backwards compatible
- **PATCH** (x.x.1): Bug fixes, backwards compatible

Examples:
- `v1.0.0` → `v1.0.1`: Bug fix
- `v1.0.1` → `v1.1.0`: New feature (memory eviction)
- `v1.1.0` → `v2.0.0`: Breaking change (API modification)

## Pre-releases

For beta/alpha releases, use:

```bash
git tag v1.1.0-beta.1
git push origin v1.1.0-beta.1
```

Mark as "pre-release" in GitHub UI after creation.

## Rollback Process

If a release has critical issues:

1. **Delete the tag locally and remotely:**
   ```bash
   git tag -d v1.1.0
   git push origin :refs/tags/v1.1.0
   ```

2. **Delete the GitHub release:**
   - Go to Releases page
   - Click "Delete" on the problematic release

3. **Fix the issue, create a new patch version:**
   ```bash
   # Fix issues in code
   git tag v1.1.1
   git push origin v1.1.1
   ```

## Platform-Specific Notes

### Linux (glibc vs musl)
- **glibc** (`x86_64-unknown-linux-gnu`): Standard Linux binary, requires glibc
- **musl** (`x86_64-unknown-linux-musl`): Static binary, no dependencies, works everywhere

### macOS Universal Binary (Optional Future Enhancement)
To create a universal binary containing both Intel and ARM:
```bash
lipo -create -output redistill \
  target/x86_64-apple-darwin/release/redistill \
  target/aarch64-apple-darwin/release/redistill
```

## CI/CD Pipeline

### On Every Push/PR (`ci.yml`)
- Run tests on Linux, macOS, Windows
- Check code formatting (`cargo fmt`)
- Run linter (`cargo clippy`)
- Security audit (`cargo audit`)

### On Tag Push (`release.yml`)
- Build release binaries for all platforms
- Create GitHub release
- Upload binaries as assets
- Generate SHA256 checksums

## Monitoring Releases

### Check Build Status
1. Go to: https://github.com/YOUR_USERNAME/redistill/actions
2. Click on the "Release" workflow
3. Monitor build progress for each platform

### Common Issues

#### Build Fails on ARM64
- Ensure cross-compilation tools are installed
- Check linker configuration in workflow

#### Binary Size Too Large
- Ensure stripping is enabled in workflow
- Check `Cargo.toml` profile settings (`strip = true`)

#### Missing Dependencies
- Update dependencies in `Cargo.toml`
- Run `cargo update` before release

## Download Statistics

To view download statistics for releases:
1. Go to: https://github.com/YOUR_USERNAME/redistill/releases
2. Each release shows download count per asset

## Example: Creating v1.1.0 Release

```bash
# 1. Update version
sed -i 's/version = "1.0.0"/version = "1.1.0"/' Cargo.toml

# 2. Update changelog
echo "## [1.1.0] - $(date +%Y-%m-%d)" >> docs/CHANGELOG.md
echo "### Added" >> docs/CHANGELOG.md
echo "- TLS/SSL support" >> docs/CHANGELOG.md

# 3. Commit changes
git add Cargo.toml docs/CHANGELOG.md
git commit -m "Release v1.1.0"
git push origin main

# 4. Create and push tag
git tag v1.1.0
git push origin v1.1.0

# 5. Wait for GitHub Actions to complete (~10-15 minutes)
# 6. Check releases page for artifacts
```

## Support

If you encounter issues with the release process:
1. Check GitHub Actions logs
2. Verify all prerequisites are met
3. Check cargo configuration
4. Open an issue if automated build fails

---

**Note**: The automated release process typically takes 10-15 minutes to build binaries for all platforms.

