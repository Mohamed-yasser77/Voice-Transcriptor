<!-- App.svelte — Tray popup & live dictation overlay -->
<script lang="ts">
  import { onMount, onDestroy } from 'svelte';
  import { listen } from '@tauri-apps/api/event';
  import TrayIcon from './lib/TrayIcon.svelte';
  import { getCurrentWindow } from '@tauri-apps/api/window';
  import { invoke } from '@tauri-apps/api/core';

  type State = 'idle' | 'recording' | 'processing';

  let state: State = 'idle';
  let lastTranscript = '';
  let currentTranscript = '';
  let errorMsg = '';
  let unlisten: (() => void) | null = null;
  let hideTimeout: ReturnType<typeof setTimeout>;

  const doHide = async () => {
    try {
      await invoke('hide_window');
    } catch {
      getCurrentWindow().hide().catch(console.error);
    }
  };

  const scheduleHide = (delay = 3500) => {
    clearTimeout(hideTimeout);
    hideTimeout = setTimeout(() => {
      if (state === 'idle') {
        doHide();
      }
    }, delay);
  };

  const closeWindow = () => {
    clearTimeout(hideTimeout);
    doHide();
  };

  onMount(async () => {
    // Initial fetch from backend so window shows current state immediately when opened
    try {
      const prev = await invoke<string>('get_last_transcript');
      if (prev) lastTranscript = prev;
      const st = await invoke<string>('get_status');
      if (st === 'recording') state = 'recording';
    } catch (e) {
      console.error('Failed to fetch initial state:', e);
    }

    unlisten = await listen<string>('pipeline-event', (event) => {
      const msg = event.payload;

      if (msg === 'RecordingStarted') {
        state = 'recording';
        errorMsg = '';
        currentTranscript = '';
        clearTimeout(hideTimeout);
        getCurrentWindow().show().catch(console.error);
      } else if (msg === 'RecordingStopped') {
        // Hotkey released: keep window open and transition to processing
        state = 'processing';
      } else if (msg === 'AudioCaptured') {
        state = 'processing';
      } else if (msg.startsWith('PartialTranscriptReady:')) {
        currentTranscript = msg.replace('PartialTranscriptReady:', '');
      } else if (msg.startsWith('FinalTranscriptReady:')) {
        currentTranscript = msg.replace('FinalTranscriptReady:', '');
        state = 'processing';
      } else if (msg.startsWith('CleanedTextReady:')) {
        lastTranscript = msg.replace('CleanedTextReady:', '');
      } else if (msg === 'InjectionComplete' || msg === 'Idle') {
        state = 'idle';
        if (currentTranscript && !lastTranscript) {
          lastTranscript = currentTranscript;
        }
        currentTranscript = '';
        scheduleHide(3500);
      } else if (msg.startsWith('Error:')) {
        state = 'idle';
        errorMsg = msg.replace('Error:', '').trim();
        scheduleHide(5000);
      }
    });
  });

  onDestroy(() => {
    unlisten?.();
    clearTimeout(hideTimeout);
  });

  const stateLabel: Record<State, string> = {
    idle: 'Ready — Hold Alt+Shift+V to dictate',
    recording: 'Listening...',
    processing: 'Transcribing & Cleaning...',
  };

  const startRecording = async () => {
    if (state === 'idle') {
      try { await invoke('start_recording'); }
      catch (err) { console.error('Failed to start recording:', err); }
    }
  };
</script>

<main class="app" data-state={state}>
  <div class="header">
    <div class="header-left" on:click={startRecording} role="button" tabindex="0">
      <TrayIcon {state} />
      <h1 class="title">Voice Transcriptor</h1>
    </div>
    <button class="close-btn" on:click={closeWindow} title="Hide overlay" aria-label="Close">
      ✕
    </button>
  </div>

  <div class="status-pill" data-state={state}>
    <span class="status-dot" />
    <span class="status-text">{stateLabel[state]}</span>
  </div>

  <div class="content-area">
    {#if currentTranscript || (state === 'processing' && currentTranscript)}
      <div class="live-transcript-box">
        <div class="typing-indicator">
          <span class="dot"></span><span class="dot"></span><span class="dot"></span>
        </div>
        <p class="live-text">{currentTranscript}</p>
      </div>
    {:else if lastTranscript}
      <div class="transcript-box">
        <p class="transcript-label">Last Injected</p>
        <p class="transcript-text">{lastTranscript}</p>
      </div>
    {:else}
      <div class="placeholder-box">
        <p class="placeholder-text">Hold <kbd>Alt</kbd> + <kbd>Shift</kbd> + <kbd>V</kbd> anywhere to dictate directly into any application.</p>
      </div>
    {/if}

    {#if errorMsg}
      <div class="error-toast" role="alert">
        ⚠ {errorMsg}
      </div>
    {/if}
  </div>

  <footer class="footer">
    <span>Hotkey:</span>
    <kbd>Alt</kbd>+<kbd>Shift</kbd>+<kbd>V</kbd>
    <span>or</span>
    <kbd>Ctrl</kbd>+<kbd>Shift</kbd>+<kbd>V</kbd>
  </footer>
</main>

<style>
  :global(*) {
    margin: 0;
    padding: 0;
    box-sizing: border-box;
  }

  :global(body) {
    background: transparent;
    font-family: 'Inter', -apple-system, BlinkMacSystemFont, "Segoe UI", Roboto, sans-serif;
    user-select: none;
    overflow: hidden;
  }

  .app {
    width: 100vw;
    height: 100vh;
    box-sizing: border-box;
    background: #12131a;
    border: 1px solid rgba(255, 255, 255, 0.12);
    border-radius: 16px;
    padding: 16px;
    color: #e8e8f0;
    display: flex;
    flex-direction: column;
    justify-content: space-between;
    box-shadow: 0 12px 36px rgba(0, 0, 0, 0.6);
    transition: border-color 0.3s ease, box-shadow 0.3s ease;
  }

  .app[data-state='recording'] {
    border-color: #ef4444aa;
    box-shadow: 0 0 24px #ef444433;
  }

  .app[data-state='processing'] {
    border-color: #8b5cf6aa;
    box-shadow: 0 0 24px #8b5cf633;
  }

  .header {
    display: flex;
    align-items: center;
    justify-content: space-between;
  }

  .header-left {
    display: flex;
    align-items: center;
    gap: 10px;
    cursor: pointer;
  }

  .title {
    font-size: 14px;
    font-weight: 700;
    letter-spacing: -0.2px;
    color: #f0f0fa;
  }

  .close-btn {
    background: transparent;
    border: none;
    color: #6b7280;
    font-size: 13px;
    cursor: pointer;
    padding: 4px 6px;
    border-radius: 6px;
    transition: all 0.2s;
  }

  .close-btn:hover {
    color: #e5e7eb;
    background: rgba(255, 255, 255, 0.08);
  }

  .status-pill {
    display: flex;
    align-items: center;
    gap: 8px;
    background: rgba(255, 255, 255, 0.05);
    border-radius: 100px;
    padding: 6px 12px;
    margin-top: 10px;
    margin-bottom: 10px;
  }

  .status-dot {
    width: 8px;
    height: 8px;
    border-radius: 50%;
    background: #6b7280;
    transition: background 0.3s, box-shadow 0.3s;
    flex-shrink: 0;
  }

  [data-state='recording'] .status-dot {
    background: #ef4444;
    box-shadow: 0 0 8px #ef4444;
    animation: pulse 1s infinite;
  }

  [data-state='processing'] .status-dot {
    background: #8b5cf6;
    box-shadow: 0 0 8px #8b5cf6;
    animation: pulse 0.6s infinite;
  }

  @keyframes pulse {
    0%, 100% { opacity: 1; }
    50% { opacity: 0.3; }
  }

  .status-text {
    font-size: 12px;
    color: #9ca3af;
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
  }

  .content-area {
    flex: 1;
    min-height: 0;
    display: flex;
    flex-direction: column;
    justify-content: center;
  }

  .transcript-box, .live-transcript-box, .placeholder-box {
    background: rgba(255, 255, 255, 0.04);
    border: 1px solid rgba(255, 255, 255, 0.08);
    border-radius: 10px;
    padding: 10px 12px;
    max-height: 100px;
    overflow-y: auto;
  }

  .live-transcript-box {
    background: rgba(139, 92, 246, 0.08);
    border-color: rgba(139, 92, 246, 0.25);
  }

  .typing-indicator {
    display: flex;
    gap: 4px;
    margin-bottom: 6px;
    align-items: center;
  }

  .typing-indicator .dot {
    width: 4px;
    height: 4px;
    background: #8b5cf6;
    border-radius: 50%;
    animation: bounce 1.4s infinite ease-in-out both;
  }

  .typing-indicator .dot:nth-child(1) { animation-delay: -0.32s; }
  .typing-indicator .dot:nth-child(2) { animation-delay: -0.16s; }

  @keyframes bounce {
    0%, 80%, 100% { transform: scale(0); }
    40% { transform: scale(1); }
  }

  .transcript-label {
    font-size: 10px;
    color: #6b7280;
    text-transform: uppercase;
    letter-spacing: 0.8px;
    margin-bottom: 4px;
  }

  .transcript-text {
    font-size: 12.5px;
    color: #d1d5db;
    line-height: 1.4;
    word-break: break-word;
  }

  .live-text {
    font-size: 13px;
    color: #f3f4f6;
    line-height: 1.4;
    font-weight: 500;
    word-break: break-word;
  }

  .placeholder-box {
    border-style: dashed;
    display: flex;
    align-items: center;
    justify-content: center;
  }

  .placeholder-text {
    font-size: 11.5px;
    color: #6b7280;
    line-height: 1.4;
    text-align: center;
  }

  .footer {
    display: flex;
    justify-content: center;
    align-items: center;
    gap: 6px;
    font-size: 11px;
    color: #4b5563;
    padding-top: 6px;
  }

  .error-toast {
    background: rgba(127, 29, 29, 0.2);
    border: 1px solid rgba(239, 68, 68, 0.4);
    border-radius: 8px;
    padding: 6px 10px;
    margin-top: 8px;
    font-size: 11px;
    color: #fca5a5;
  }

  kbd {
    background: #1f2028;
    border: 1px solid #374151;
    border-radius: 4px;
    padding: 1px 5px;
    font-size: 10px;
    color: #9ca3af;
  }

  /* Custom subtle scrollbar */
  ::-webkit-scrollbar {
    width: 4px;
  }
  ::-webkit-scrollbar-thumb {
    background: rgba(255, 255, 255, 0.15);
    border-radius: 4px;
  }
</style>
