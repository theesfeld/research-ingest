const status = document.getElementById("status");

chrome.runtime.sendMessage({ type: "health" }, (resp) => {
  if (chrome.runtime.lastError) {
    status.textContent = chrome.runtime.lastError.message;
    status.className = "bad";
    return;
  }
  if (resp?.ok) {
    status.textContent =
      "Daemon up. Click Send, or use Ctrl+Shift+Y / right-click.";
    status.className = "ok";
  } else {
    status.textContent =
      "Daemon down. On the machine run:\nresearch-ingest enable";
    status.className = "bad";
  }
});

document.getElementById("send").addEventListener("click", () => {
  status.textContent = "Sending…";
  chrome.runtime.sendMessage({ type: "send-active" }, (resp) => {
    if (chrome.runtime.lastError) {
      status.textContent = chrome.runtime.lastError.message;
      status.className = "bad";
      return;
    }
    if (resp?.ok) {
      status.textContent = "Sent. Background processing continues.";
      status.className = "ok";
    } else {
      status.textContent = resp?.error || "Failed";
      status.className = "bad";
    }
  });
});
