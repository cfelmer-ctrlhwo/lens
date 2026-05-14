// Purpose: Top-of-page dismissible red banner shown whenever an IPC call throws.
// Process: App.tsx aggregates errors from the timeline + app-status hooks into a single string; passes it here.
//          The X button calls onDismiss to clear it. If `message` is null the banner renders nothing.
// Connections: Rendered by App.tsx. Styled by App.css (.lens-error-banner.*).

type Props = {
  message: string | null;
  onDismiss: () => void;
};

export function ErrorBanner({ message, onDismiss }: Props) {
  if (!message) return null;
  return (
    <div className="lens-error-banner" role="alert">
      <span className="lens-error-banner__label">Error</span>
      <span className="lens-error-banner__message">{message}</span>
      <button
        type="button"
        className="lens-error-banner__dismiss"
        onClick={onDismiss}
        aria-label="Dismiss error"
      >
        ×
      </button>
    </div>
  );
}
