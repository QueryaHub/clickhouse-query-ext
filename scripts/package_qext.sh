#!/usr/bin/env bash
set -eo pipefail

# Script to package clickhouse-query-ext binary along with manifest.json and assets/
# into a distribution .qext archive (.zip) and generate SHA-256 checksums for Querya Desktop Block B validation.

TARGET=""
BIN_PATH=""

while [[ "$#" -gt 0 ]]; do
  case $1 in
    --target) TARGET="$2"; shift 2 ;;
    --bin) BIN_PATH="$2"; shift 2 ;;
    *) echo "Unknown parameter passed: $1"; exit 1 ;;
  esac
done

if [ -z "$TARGET" ]; then
  TARGET=$(rustc -vV | sed -n 's|host: ||p' || echo "x86_64-unknown-linux-gnu")
fi

if [ -z "$BIN_PATH" ]; then
  # Try target-specific path first, then fallback to target/release/
  if [[ "$TARGET" == *"windows"* ]]; then
    BIN_NAME="clickhouse-query-ext.exe"
    DEST_BIN_NAME="clickhouse_rpc_server.exe"
  else
    BIN_NAME="clickhouse-query-ext"
    DEST_BIN_NAME="clickhouse_rpc_server"
  fi

  if [ -f "target/${TARGET}/release/${BIN_NAME}" ]; then
    BIN_PATH="target/${TARGET}/release/${BIN_NAME}"
  elif [ -f "target/release/${BIN_NAME}" ]; then
    BIN_PATH="target/release/${BIN_NAME}"
  else
    echo "❌ Error: Could not find built binary for ${TARGET}."
    echo "   Please run './scripts/build_cross.sh ${TARGET}' first or specify '--bin <path>'."
    exit 1
  fi
else
  if [[ "$TARGET" == *"windows"* ]] || [[ "$BIN_PATH" == *".exe" ]]; then
    DEST_BIN_NAME="clickhouse_rpc_server.exe"
  else
    DEST_BIN_NAME="clickhouse_rpc_server"
  fi
fi

# Extract version from manifest.json using Python (works reliably across platforms)
VERSION=$(python3 -c "import json; print(json.load(open('manifest.json'))['version'])" 2>/dev/null || echo "1.0.0")

echo "=========================================================================="
echo "📦 Packaging Querya Extension (.qext) for target: ${TARGET}"
echo "   Binary source:  ${BIN_PATH}"
echo "   Extension v:    ${VERSION}"
echo "=========================================================================="

mkdir -p dist
STAGING_DIR="dist/staging-${TARGET}"
rm -rf "${STAGING_DIR}"
mkdir -p "${STAGING_DIR}/bin" "${STAGING_DIR}/assets"

# 1. Copy manifest and assets
cp manifest.json "${STAGING_DIR}/"
cp -r assets/* "${STAGING_DIR}/assets/"

# 2. Copy binary into bin/ under both the manifest main entry and original name
cp "${BIN_PATH}" "${STAGING_DIR}/bin/${DEST_BIN_NAME}"
if [ "${DEST_BIN_NAME}" != "${BIN_NAME}" ] && [ ! -f "${STAGING_DIR}/bin/${BIN_NAME}" ]; then
  cp "${BIN_PATH}" "${STAGING_DIR}/bin/${BIN_NAME}"
fi

# 3. Create .qext (.zip) archive using python zipfile module or zip CLI
ARCHIVE_NAME="clickhouse-query-ext-${VERSION}-${TARGET}.qext"
ARCHIVE_PATH="dist/${ARCHIVE_NAME}"
rm -f "${ARCHIVE_PATH}"

python3 -c "
import zipfile, os, sys
archive_path = sys.argv[1]
staging_dir = sys.argv[2]
with zipfile.ZipFile(archive_path, 'w', zipfile.ZIP_DEFLATED) as zipf:
    for root, dirs, files in os.walk(staging_dir):
        for file in files:
            full_path = os.path.join(root, file)
            rel_path = os.path.relpath(full_path, staging_dir)
            zipf.write(full_path, rel_path)
" "${ARCHIVE_PATH}" "${STAGING_DIR}"

# Also create universal/shorthand name if packaging host target
HOST_TARGET=$(rustc -vV | sed -n 's|host: ||p' 2>/dev/null || echo "")
if [ "$TARGET" = "$HOST_TARGET" ]; then
  cp "${ARCHIVE_PATH}" "dist/clickhouse-query-ext-${VERSION}.qext"
fi

# 4. Generate SHA-256 checksums
generate_sha256() {
  local file_path="$1"
  local sha_output="${file_path}.sha256"
  python3 -c "
import hashlib, sys, os
with open(sys.argv[1], 'rb') as f:
    print(hashlib.sha256(f.read()).hexdigest() + ' *' + os.path.basename(sys.argv[1]))
" "${file_path}" > "${sha_output}"
  echo "🔒 Checksum generated: ${sha_output} -> $(cat "${sha_output}")"
}

generate_sha256 "${ARCHIVE_PATH}"
if [ -f "dist/clickhouse-query-ext-${VERSION}.qext" ] && [ "$TARGET" = "$HOST_TARGET" ]; then
  generate_sha256 "dist/clickhouse-query-ext-${VERSION}.qext"
fi

rm -rf "${STAGING_DIR}"

SIZE=$(ls -lh "${ARCHIVE_PATH}" | awk '{print $5}')
echo ""
echo "✅ Packaging successfully completed!"
echo "   Created archive: ${ARCHIVE_PATH} (Size: ${SIZE})"
echo "   Ready for deployment and Block B security verification."
