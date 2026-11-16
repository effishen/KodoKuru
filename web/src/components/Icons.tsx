// Small inline icons (stroke-based, inherit currentColor).

type P = { size?: number; className?: string };

const S = ({ size = 16, className, children }: P & { children: React.ReactNode }) => (
  <svg
    width={size}
    height={size}
    viewBox="0 0 24 24"
    fill="none"
    stroke="currentColor"
    strokeWidth={1.8}
    strokeLinecap="round"
    strokeLinejoin="round"
    className={className}
    aria-hidden
  >
    {children}
  </svg>
);

export const Logo = ({ size = 22 }: P) => (
  <svg width={size} height={size} viewBox="0 0 32 32" aria-hidden>
    <circle cx="16" cy="16" r="13" fill="var(--accent)" />
    <circle cx="16" cy="16" r="5" fill="var(--bg)" />
    <circle cx="16" cy="16" r="9" fill="none" stroke="var(--bg)" strokeWidth="1.5" strokeDasharray="3 3" />
  </svg>
);

export const Plus = (p: P) => (
  <S {...p}>
    <path d="M12 5v14M5 12h14" />
  </S>
);
export const Send = (p: P) => (
  <S {...p}>
    <path d="M12 19V5M5 12l7-7 7 7" />
  </S>
);
export const StopIcon = (p: P) => (
  <S {...p}>
    <rect x="6" y="6" width="12" height="12" rx="2" fill="currentColor" />
  </S>
);
export const Gear = (p: P) => (
  <S {...p}>
    <circle cx="12" cy="12" r="3" />
    <path d="M19.4 15a1.7 1.7 0 0 0 .3 1.8l.1.1a2 2 0 1 1-2.8 2.8l-.1-.1a1.7 1.7 0 0 0-1.8-.3 1.7 1.7 0 0 0-1 1.5V21a2 2 0 1 1-4 0v-.1a1.7 1.7 0 0 0-1.1-1.5 1.7 1.7 0 0 0-1.8.3l-.1.1a2 2 0 1 1-2.8-2.8l.1-.1a1.7 1.7 0 0 0 .3-1.8 1.7 1.7 0 0 0-1.5-1H3a2 2 0 1 1 0-4h.1a1.7 1.7 0 0 0 1.5-1.1 1.7 1.7 0 0 0-.3-1.8l-.1-.1a2 2 0 1 1 2.8-2.8l.1.1a1.7 1.7 0 0 0 1.8.3H9a1.7 1.7 0 0 0 1-1.5V3a2 2 0 1 1 4 0v.1a1.7 1.7 0 0 0 1 1.5 1.7 1.7 0 0 0 1.8-.3l.1-.1a2 2 0 1 1 2.8 2.8l-.1.1a1.7 1.7 0 0 0-.3 1.8V9a1.7 1.7 0 0 0 1.5 1H21a2 2 0 1 1 0 4h-.1a1.7 1.7 0 0 0-1.5 1z" />
  </S>
);
export const LockIcon = (p: P) => (
  <S {...p}>
    <rect x="5" y="11" width="14" height="10" rx="2" />
    <path d="M8 11V7a4 4 0 0 1 8 0v4" />
  </S>
);
export const Chevron = ({ open, ...p }: P & { open?: boolean }) => (
  <S {...p} className={`chev ${open ? "open" : ""}`}>
    <path d="M9 6l6 6-6 6" />
  </S>
);
export const Tool = (p: P) => (
  <S {...p}>
    <path d="M14.7 6.3a4 4 0 0 0-5.4 5.4L3 18v3h3l6.3-6.3a4 4 0 0 0 5.4-5.4l-2.5 2.5-2.4-.6-.6-2.4z" />
  </S>
);
export const Brain = (p: P) => (
  <S {...p}>
    <path d="M9 4a3 3 0 0 0-3 3 3 3 0 0 0-2 5 3 3 0 0 0 2 5 3 3 0 0 0 6 1V5a2 2 0 0 0-3-1zM15 4a3 3 0 0 1 3 3 3 3 0 0 1 2 5 3 3 0 0 1-2 5 3 3 0 0 1-6 1" />
  </S>
);
export const Panel = (p: P) => (
  <S {...p}>
    <rect x="3" y="4" width="18" height="16" rx="2" />
    <path d="M15 4v16" />
  </S>
);
export const Users = (p: P) => (
  <S {...p}>
    <circle cx="9" cy="8" r="3.2" />
    <path d="M3 20c0-3.3 2.7-5.5 6-5.5s6 2.2 6 5.5M16 4.5a3 3 0 0 1 0 6M21 20c0-2.6-1.5-4.5-3.8-5.2" />
  </S>
);
export const Folder = (p: P) => (
  <S {...p}>
    <path d="M3 7a2 2 0 0 1 2-2h4l2 2h8a2 2 0 0 1 2 2v8a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2z" />
  </S>
);
export const FileIcon = (p: P) => (
  <S {...p}>
    <path d="M14 3H7a2 2 0 0 0-2 2v14a2 2 0 0 0 2 2h10a2 2 0 0 0 2-2V8z" />
    <path d="M14 3v5h5" />
  </S>
);
export const Check = (p: P) => (
  <S {...p}>
    <path d="M5 12l5 5 9-10" />
  </S>
);
export const Close = (p: P) => (
  <S {...p}>
    <path d="M6 6l12 12M18 6L6 18" />
  </S>
);
export const Activity = (p: P) => (
  <S {...p}>
    <path d="M3 12h4l3-8 4 16 3-8h4" />
  </S>
);
