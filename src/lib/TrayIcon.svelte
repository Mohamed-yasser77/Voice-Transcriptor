<!-- TrayIcon.svelte — Animated microphone icon that reflects pipeline state -->
<script lang="ts">
  export let state: 'idle' | 'recording' | 'processing' = 'idle';
</script>

<div class="icon-wrap" data-state={state}>
  <svg
    viewBox="0 0 24 24"
    fill="none"
    xmlns="http://www.w3.org/2000/svg"
    class="mic-icon"
    aria-label={`Microphone — ${state}`}
  >
    <!-- Microphone body -->
    <rect x="9" y="2" width="6" height="11" rx="3" fill="currentColor" />
    <!-- Stand -->
    <path
      d="M5 10a7 7 0 0 0 14 0"
      stroke="currentColor"
      stroke-width="1.8"
      stroke-linecap="round"
    />
    <line x1="12" y1="17" x2="12" y2="21" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" />
    <line x1="9" y1="21" x2="15" y2="21" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" />
  </svg>

  {#if state === 'recording'}
    <span class="ripple" />
    <span class="ripple ripple--delayed" />
  {/if}
</div>

<style>
  .icon-wrap {
    position: relative;
    width: 32px;
    height: 32px;
    display: flex;
    align-items: center;
    justify-content: center;
    flex-shrink: 0;
  }

  .mic-icon {
    width: 20px;
    height: 20px;
    color: #6b7280;
    transition: color 0.3s ease;
    position: relative;
    z-index: 1;
  }

  [data-state='recording'] .mic-icon {
    color: #ef4444;
  }

  [data-state='processing'] .mic-icon {
    color: #8b5cf6;
  }

  .ripple {
    position: absolute;
    inset: 0;
    border-radius: 50%;
    border: 1.5px solid #ef4444;
    opacity: 0;
    animation: ripple-out 1.4s ease-out infinite;
  }

  .ripple--delayed {
    animation-delay: 0.7s;
  }

  @keyframes ripple-out {
    0%   { transform: scale(0.6); opacity: 0.6; }
    100% { transform: scale(1.4); opacity: 0; }
  }
</style>
