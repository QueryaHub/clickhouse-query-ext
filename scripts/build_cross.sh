#!/usr/bin/env bash
set -eo pipefail

# Script to cross-compile clickhouse-query-ext for the 3 supported Querya Desktop targets:
# - x86_64-unknown-linux-gnu (Linux x86_64)
# - aarch64-apple-darwin (macOS Apple Silicon)
# - x86_64-pc-windows-msvc / x86_64-pc-windows-gnu (Windows x86_64)

TARGETS=(
  "x86_64-unknown-linux-gnu"
  "aarch64-apple-darwin"
  "x86_64-pc-windows-msvc"
)

# Determine the native host target
HOST_TARGET=$(rustc -vV | sed -n 's|host: ||p')
BUILD_CMD="cargo"

# Check if cross is installed and requested, or use cargo directly
if command -v cross &> /dev/null && [ "$USE_CROSS" = "1" ]; then
  BUILD_CMD="cross"
fi

echo "=========================================================================="
echo "🎯 Starting release build / cross-compilation for clickhouse-query-ext..."
echo "   Build command: ${BUILD_CMD}"
echo "   Host target:   ${HOST_TARGET}"
echo "=========================================================================="

# If arguments are passed, use them as target list; otherwise build either requested target or all available
if [ "$#" -gt 0 ]; then
  SELECTED_TARGETS=("$@")
else
  # If running locally without cross, build only the host target by default unless --all is passed
  if [ "$BUILD_CMD" = "cargo" ] && [ "$1" != "--all" ]; then
    echo "ℹ️ Running on local host without 'cross' CLI specified. Building for native target: ${HOST_TARGET}"
    SELECTED_TARGETS=("$HOST_TARGET")
  else
    SELECTED_TARGETS=("${TARGETS[@]}")
  fi
fi

for TARGET in "${SELECTED_TARGETS[@]}"; do
  if [ "$TARGET" = "--all" ]; then
    continue
  fi

  echo ""
  echo ">>> 🔨 Building target: ${TARGET} ..."
  
  # Check if target is installed when using standard cargo
  if [ "$BUILD_CMD" = "cargo" ] && [ "$TARGET" != "$HOST_TARGET" ]; then
    if ! rustup target list | grep "${TARGET} (installed)" > /dev/null; then
      echo "⚠️ Target ${TARGET} not installed in rustup toolchain. Attempting to install..."
      rustup target add "${TARGET}" || {
        echo "❌ Failed to add target ${TARGET}. Skipping or please install target toolchain / 'cross'."
        continue
      }
    fi
  fi

  # Execute build
  "${BUILD_CMD}" build --release --target "${TARGET}"

  # Verify output binary
  BIN_NAME="clickhouse-query-ext"
  if [[ "${TARGET}" == *"windows"* ]]; then
    BIN_NAME="clickhouse-query-ext.exe"
  fi

  OUT_PATH="target/${TARGET}/release/${BIN_NAME}"
  if [ -f "${OUT_PATH}" ]; then
    SIZE=$(ls -lh "${OUT_PATH}" | awk '{print $5}')
    echo "✅ Successfully built ${OUT_PATH} (Size: ${SIZE})"
  else
    echo "⚠️ Warning: expected binary ${OUT_PATH} not found after build."
  fi
done

echo ""
echo "🎉 Cross-compilation process completed successfully!"
