# ComfyUI Communication Optimizations

This document outlines potential optimizations for the communication bridge between `zun-rust-server` and the ComfyUI backend.

## 1. WebSocket Integration (Reactive Completion)

**Current State:** The worker polls `/history/{prompt_id}` every 1000ms.
**Proposed Change:** Implement a WebSocket client connecting to ComfyUI's `/ws` endpoint.
**Benefits:**
- **Zero Latency:** The worker is notified immediately when a node finishes or a prompt is completed.
- **Reduced Overhead:** Eliminates the "empty" polling requests that occur during long-running generations (e.g., FLUX).
- **Rich Feedback:** Enables streaming progress percentages (e.g., "Step 15/20") back to the Android client.

## 2. Shared Filesystem (Local-Only Optimization)

**Current State:** Images are sent/received via HTTP multipart uploads and downloads.
**Proposed Change:** If both services run on the same host, use a shared Docker volume or symlinked directory.
**Benefits:**
- **Instant Transfers:** Moving an image becomes a filesystem `mv` or `cp` (or just passing a path) instead of a multi-megabyte HTTP transfer.
- **Reduced Memory:** Avoids buffering large image bytes in the Rust process's RAM.

## 3. API-Format Workflows

**Current State:** The server likely uses standard UI-formatted JSON workflows.
**Proposed Change:** Export workflows from ComfyUI using the "API Format" (Save -> Export API JSON).
**Benefits:**
- **Compact Payload:** API JSON is significantly smaller as it removes all coordinate and UI metadata.
- **Faster Parsing:** ComfyUI processes these faster as they map directly to the execution graph.

## 4. Model Warm-starting & VRAM Management

**Current State:** ComfyUI might unload models if idle or if another process requests VRAM.
**Proposed Change:**
- **Keep-alive:** Optionally send a "no-op" or tiny generation request every N minutes to keep models resident in VRAM.
- **Model Pinning:** Standardize on a specific base model across multiple prompt templates to avoid "model swapping" lag when switching between jobs.

## 5. Thumbnail-First Retrieval

**Current State:** The worker downloads the full-resolution result immediately.
**Proposed Change:** 
- If the workflow produces multiple nodes (e.g., a preview and a high-res save), download the preview/thumbnail first.
- Update the job status to `done` so the client can show the result immediately, while the full-res download finishes in the background.

## 6. Batching Strategy (Future)

**Current State:** Serial execution (1 job at a time).
**Proposed Change:** If the GPU has sufficient VRAM (e.g., 24GB+), allow parallel submission for lighter workflows while keeping heavy models (FLUX) serial.
