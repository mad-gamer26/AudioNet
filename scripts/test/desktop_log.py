"""Reads the Windows desktop app's status log the way a UI Automation
client would: invokes the main window's "Status log…" button, reads the
"Status log" edit in the "AudioNet Status Log" window, and closes that
window with its Close button (focus goes back to the main window)."""
import time

TITLE = "AudioNet Status Log"


def _descendants(uia, UIA, el):
    arr = el.FindAll(UIA.TreeScope_Descendants, uia.CreateTrueCondition())
    return [arr.GetElement(i) for i in range(arr.Length)]


def _invoke(UIA, el):
    el.GetCurrentPattern(UIA.UIA_InvokePatternId).QueryInterface(UIA.IUIAutomationInvokePattern).Invoke()


def window(uia, UIA, pid, main=None):
    """The open status log window of process `pid`, or None. UI Automation
    lists an owned window under its owner (`main`) or at the top level."""
    cond = uia.CreateAndCondition(uia.CreatePropertyCondition(UIA.UIA_ProcessIdPropertyId, pid),
                                  uia.CreatePropertyCondition(UIA.UIA_NamePropertyId, TITLE))
    return ((main.FindFirst(UIA.TreeScope_Children, cond) if main else None)
            or uia.GetRootElement().FindFirst(UIA.TreeScope_Children, cond))


def open_log(uia, UIA, main):
    """Opens the status log from the main window; returns its window."""
    pid = main.CurrentProcessId
    w = window(uia, UIA, pid, main)
    if w:
        return w
    button = next(c for c in _descendants(uia, UIA, main) if c.CurrentControlType == UIA.UIA_ButtonControlTypeId
                  and (c.CurrentName or "").replace("&", "").startswith("Status log"))
    _invoke(UIA, button)
    end = time.time() + 5
    while time.time() < end:
        w = window(uia, UIA, pid, main)
        if w:
            return w
        time.sleep(0.1)
    raise RuntimeError("the status log window did not open")


def text_of(uia, UIA, log_window):
    edit = next(c for c in _descendants(uia, UIA, log_window) if c.CurrentControlType == UIA.UIA_EditControlTypeId
                and (c.CurrentName or "").replace("&", "").startswith("Status log"))
    return edit.GetCurrentPattern(UIA.UIA_ValuePatternId).QueryInterface(UIA.IUIAutomationValuePattern).CurrentValue


def close(uia, UIA, log_window):
    button = next(c for c in _descendants(uia, UIA, log_window) if c.CurrentControlType == UIA.UIA_ButtonControlTypeId
                  and c.CurrentName == "Close")
    _invoke(UIA, button)


def read(uia, UIA, main):
    """The whole status log (opens and closes the window)."""
    w = open_log(uia, UIA, main)
    try:
        return text_of(uia, UIA, w)
    finally:
        close(uia, UIA, w)
