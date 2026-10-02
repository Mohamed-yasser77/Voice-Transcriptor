import time
from pywinauto import Application
import pywinauto.keyboard as kb

def test_voice_dictate_hotkey():
    print("VoiceDictate is running as a tray app. Triggering global hotkey...")
    
    print("Simulating global hotkey (Alt+Shift+V)...")
    # Open notepad to inject into
    notepad = Application(backend="uia").start("notepad.exe")
    time.sleep(1)
    
    # Trigger hotkey
    kb.send_keys('%+v') 
    print("Hotkey triggered. The app should now be recording...")
    
    # Wait for the recording to stop (silence detection) and injection
    time.sleep(5)
    
    print("End-to-end hotkey dispatch and injection test complete.")
    notepad.kill()

if __name__ == "__main__":
    test_voice_dictate_hotkey()
