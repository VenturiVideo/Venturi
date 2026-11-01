# Native panel windows

Media, Effects, Timeline, Viewer and Properties can be detached with the Detach
button inside the panel. Audio Mixer, Color and Animation Curves offer the same
action in their window controls. Reattach returns a native window to its
original location in the editor. Closing Timeline or Viewer reattaches it;
closing other panels hides them. The toolbar toggles reopen them, and Properties
also appears in the View menu. Detached state and window size are saved in the
application settings.

Editing uses the same project and undo history across windows. Keyboard actions
from a focused detached window are forwarded to the editor unless a text field
or popup owns them. Media and effect payloads can be dragged between native
viewports when the backend provides desktop window coordinates. A release is
delivered to at most one destination; Escape, closing the source, or releasing
outside the application cancels the transfer.

The current bridge needs desktop coordinates, which Wayland backends may not
provide. It also cannot determine native stacking order between overlapping
child windows: ambiguous destinations are rejected. Keep destination windows
uncovered when transferring a payload. Physical mixed-DPI displays, monitor
removal and native Wayland interaction still require acceptance testing.

Backends that embed viewports keep the panels in the editor. This feature is
an extension of the existing detachable-panel proposal #99 for issue #76.
