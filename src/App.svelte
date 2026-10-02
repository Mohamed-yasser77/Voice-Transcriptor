<!-- App.svelte — Main tray popup UI -->
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

  const scheduleHide = () => {
    clearTimeout(hideTimeout);
    hideTimeout = setTimeout(() => {
      if (state === 'idle') {
        getCurrentWindow().hide().catch(console.error);
      }
    }, 4000);
  };

  onMount(async () => {
    unlisten = await listen<string>('pipeline-event', (event) => {
      const msg = event.payload;

      if (msg === 'RecordingStarted') {
        state = 'recording';
        errorMsg = '';
        clearTimeout(hideTimeout);
      } else if (msg === 'RecordingStopped') {
        state = 'idle';
        getCurrentWindow().hide().catch(console.error);
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
        if (currentTranscript) {
          lastTranscript = currentTranscript;
        }
        currentTranscript = '';
        scheduleHide();
      } else if (msg.startsWith('Error:')) {
        state = 'idle';
        errorMsg = msg.replace('Error:', '').trim();
        scheduleHide();
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
    processing: 'Processing...',
  };

  const startRecording = async () => {
    if (state === 'idle') {
      try { await invoke('start_recording'); }
      catch (err) { console.error('Failed to start recording:', err); }
    }
  };
</script>

<main class="app" data-state={state} on:click={startRecording} style="cursor: pointer;">
  <div class="header">
    <TrayIcon {state} />
    <h1 class="title">VoiceDictate</h1>
  </div>

  <div class="status-pill" data-state={state}>
    <span class="status-dot" />
    <span class="status-text">{stateLabel[state]}</span>
  </div>

  {#if currentTranscript || (state === 'processing' && currentTranscript)}
    <div class="live-transcript-box">
      <div class="typing-indicator">
        <span class="dot"></span><span class="dot"></span><span class="dot"></span>
      </div>
      <p class="live-text">{currentTranscript}</p>
    </div>
  {:else if lastTranscript}
    <div class="transcript-box">
      <p class="transcript-label">Last injected</p>
      <p class="transcript-text">{lastTranscript}</p>
    </div>
  {/if}

  {#if errorMsg}
    <div class="error-toast" role="alert">
      ⚠ {errorMsg}
    </div>
  {/if}

  <footer class="footer">
    <kbd>Alt</kbd> + <kbd>Shift</kbd> + <kbd>V</kbd>
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
    font-family: 'Inter', system-ui, sans-serif;
  }

  .app {
    width: 300px;
    background: #0f0f14;
    border: 1px solid #ffffff14;
    border-radius: 16px;
    padding: 20px;
    color: #e8e8f0;
    backdrop-filter: blur(20px);
    transition: all 0.3s ease;
  }

  .app[data-state='recording'] {
    border-color: #ef4444aa;
    box-shadow: 0 0 24px #ef444422;
  }

  .app[data-state='processing'] {
    border-color: #8b5cf6aa;
    box-shadow: 0 0 24px #8b5cf622;
  }

  .header {
    display: flex;
    align-items: center;
    gap: 10px;
    margin-bottom: 16px;
  }

  .title {
    font-size: 15px;
    font-weight: 700;
    letter-spacing: -0.3px;
    color: #f0f0fa;
  }

  .status-pill {
    display: flex;
    align-items: center;
    gap: 8px;
    background: #ffffff0a;
    border-radius: 100px;
    padding: 7px 14px;
    margin-bottom: 16px;
    transition: background 0.3s;
  }

  .status-dot {
    width: 8px;
    height: 8px;
    border-radius: 50%;
    background: #6b7280;
    transition: background 0.3s, box-shadow 0.3s;
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
    50% { opacity: 0.4; }
  }

  .status-text {
    font-size: 13px;
    color: #9ca3af;
  }

  .transcript-box, .live-transcript-box {
    background: #ffffff08;
    border: 1px solid #ffffff0f;
    border-radius: 12px;
    padding: 14px 16px;
    margin-bottom: 16px;
  }

  .live-transcript-box {
    background: rgba(139, 92, 246, 0.05);
    border-color: rgba(139, 92, 246, 0.2);
    box-shadow: 0 4px 20px rgba(139, 92, 246, 0.05);
    position: relative;
    overflow: hidden;
  }

  .typing-indicator {
    display: flex;
    gap: 4px;
    margin-bottom: 8px;
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
    margin-bottom: 6px;
  }

  .transcript-text {
    font-size: 13px;
    color: #d1d5db;
    line-height: 1.5;
  }

  .live-text {
    font-size: 14px;
    color: #f3f4f6;
    line-height: 1.5;
    font-weight: 500;
  }

  .footer {
    display: flex;
    justify-content: center;
    align-items: center;
    gap: 4px;
    font-size: 11px;
    color: #374151;
  }

  /* ── Toggle CSS removed (Voice-Only mode is fixed) ── */

  .error-toast {
    background: #7f1d1d22;
    border: 1px solid #ef444455;
    border-radius: 8px;
    padding: 8px 12px;
    margin-bottom: 12px;
    font-size: 12px;
    color: #fca5a5;
    animation: fadeIn 0.2s ease;
  }

  @keyframes fadeIn {
    from { opacity: 0; transform: translateY(-4px); }
    to   { opacity: 1; transform: translateY(0); }
  }

  kbd {
    background: #1f2028;
    border: 1px solid #374151;
    border-radius: 4px;
    padding: 2px 6px;
    font-size: 10px;
    font-family: inherit;
    color: #6b7280;
  }
</style>
