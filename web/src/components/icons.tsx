import type { SVGProps } from "react";

// Line icons on a 20×20 grid: 1.6 stroke, round caps and joins, currentColor.
const paths = {
  overview: (
    <>
      <rect x="3" y="3" width="6" height="6" rx="1.5" />
      <rect x="11" y="3" width="6" height="4" rx="1.5" />
      <rect x="11" y="9" width="6" height="8" rx="1.5" />
      <rect x="3" y="11" width="6" height="6" rx="1.5" />
    </>
  ),
  server: (
    <>
      <rect x="3" y="3.5" width="14" height="5.5" rx="1.5" />
      <rect x="3" y="11" width="14" height="5.5" rx="1.5" />
      <path d="M6 6.25h.01M6 13.75h.01M9 6.25h5M9 13.75h5" />
    </>
  ),
  folder: <path d="M3 6.5A1.5 1.5 0 0 1 4.5 5h3l1.5 1.5h6.5A1.5 1.5 0 0 1 17 8v6.5a1.5 1.5 0 0 1-1.5 1.5h-11A1.5 1.5 0 0 1 3 14.5Z" />,
  layers: (
    <>
      <path d="m10 3 7 3.75L10 10.5 3 6.75Z" />
      <path d="m3 10.25 7 3.75 7-3.75" />
      <path d="m3 13.75 7 3.75 7-3.75" />
    </>
  ),
  activity: <path d="M2.5 10h3l2-5.5 5 11 2-5.5h3" />,
  bell: (
    <>
      <path d="M5 8.5a5 5 0 0 1 10 0c0 4 1.5 5.5 1.5 5.5h-13S5 12.5 5 8.5Z" />
      <path d="M8.5 16.5a1.6 1.6 0 0 0 3 0" />
    </>
  ),
  logs: (
    <>
      <rect x="3" y="3.5" width="14" height="13" rx="2" />
      <path d="m6.5 8 2 2-2 2M10.5 12.5h3" />
    </>
  ),
  archive: (
    <>
      <rect x="2.5" y="3.5" width="15" height="4" rx="1" />
      <path d="M4 7.5v7.5a1.5 1.5 0 0 0 1.5 1.5h9A1.5 1.5 0 0 0 16 15V7.5M8 11h4" />
    </>
  ),
  bucket: (
    <>
      <ellipse cx="10" cy="5" rx="6.5" ry="2" />
      <path d="M3.5 5 5 15.5c.2 1 2.3 1.5 5 1.5s4.8-.5 5-1.5L16.5 5" />
    </>
  ),
  zap: <path d="M11 2.5 4.5 11H10l-1 6.5L15.5 9H10Z" />,
  shield: (
    <>
      <path d="M10 2.5 16 5v4.5c0 3.8-2.6 6.8-6 8-3.4-1.2-6-4.2-6-8V5Z" />
      <path d="m7.5 10 1.75 1.75L12.75 8.25" />
    </>
  ),
  box: (
    <>
      <path d="m10 2.5 6.5 3.5v8L10 17.5 3.5 14V6Z" />
      <path d="m3.5 6 6.5 3.5L16.5 6M10 9.5v8" />
    </>
  ),
  git: (
    <>
      <circle cx="6" cy="5" r="1.75" />
      <circle cx="6" cy="15" r="1.75" />
      <circle cx="14" cy="7" r="1.75" />
      <path d="M6 6.75v6.5M14 8.75c0 3-3 3.5-6.5 5" />
    </>
  ),
  users: (
    <>
      <circle cx="8" cy="7" r="3" />
      <path d="M2.5 16.5c.6-2.6 2.8-4 5.5-4s4.9 1.4 5.5 4" />
      <path d="M13 4.2a3 3 0 0 1 0 5.6M15 12.8c1.3.6 2.2 1.9 2.5 3.7" />
    </>
  ),
  key: (
    <>
      <circle cx="7" cy="13" r="3.5" />
      <path d="m9.5 10.5 7-7M14 6l2 2M12 8l1.5 1.5" />
    </>
  ),
  history: (
    <>
      <path d="M3.5 10a6.5 6.5 0 1 0 1.9-4.6L3.5 7.3" />
      <path d="M3.5 3.5v3.8h3.8M10 6.5V10l2.5 1.5" />
    </>
  ),
  settings: (
    <>
      <circle cx="10" cy="10" r="2.5" />
      <path d="M10 2.5v2M10 15.5v2M17.5 10h-2M4.5 10h-2M15.3 4.7l-1.4 1.4M6.1 13.9l-1.4 1.4M15.3 15.3l-1.4-1.4M6.1 6.1 4.7 4.7" />
    </>
  ),
  user: (
    <>
      <circle cx="10" cy="7" r="3.25" />
      <path d="M4 17c.7-3 3-4.5 6-4.5s5.3 1.5 6 4.5" />
    </>
  ),
  sun: (
    <>
      <circle cx="10" cy="10" r="3.25" />
      <path d="M10 2.5v1.5M10 16v1.5M17.5 10H16M4 10H2.5M15.3 4.7l-1.05 1.05M5.75 14.25 4.7 15.3M15.3 15.3l-1.05-1.05M5.75 5.75 4.7 4.7" />
    </>
  ),
  moon: <path d="M16.5 12A6.5 6.5 0 0 1 8 3.5a6.5 6.5 0 1 0 8.5 8.5Z" />,
  logout: (
    <>
      <path d="M8 3.5H5A1.5 1.5 0 0 0 3.5 5v10A1.5 1.5 0 0 0 5 16.5h3" />
      <path d="m12.5 6.5 3.5 3.5-3.5 3.5M16 10H8" />
    </>
  ),
  menu: <path d="M3.5 6h13M3.5 10h13M3.5 14h13" />,
  close: <path d="m5 5 10 10M15 5 5 15" />,
  plus: <path d="M10 4v12M4 10h12" />,
  search: (
    <>
      <circle cx="9" cy="9" r="5.5" />
      <path d="m13 13 3.5 3.5" />
    </>
  ),
  chevronRight: <path d="m8 5 5 5-5 5" />,
  arrowRight: <path d="M4 10h12M11.5 5.5 16 10l-4.5 4.5" />,
  check: <path d="m4.5 10.5 3.5 3.5 7.5-8" />,
  alert: (
    <>
      <path d="M8.7 3.3a1.5 1.5 0 0 1 2.6 0l6 10.5A1.5 1.5 0 0 1 16 16H4a1.5 1.5 0 0 1-1.3-2.2Z" />
      <path d="M10 8v3.25M10 13.75h.01" />
    </>
  ),
  info: (
    <>
      <circle cx="10" cy="10" r="7" />
      <path d="M10 9v4.5M10 6.5h.01" />
    </>
  ),
  rocket: (
    <>
      <path d="M11.5 14.5 8 16l-.5-3.5L4 12l1.5-3.5C8 4 12 2.5 17.5 2.5c0 5.5-1.5 9.5-6 12Z" />
      <circle cx="12.5" cy="7.5" r="1.5" />
    </>
  ),
} as const;

export type IconName = keyof typeof paths;

export function Icon({
  name,
  size = 16,
  ...props
}: { name: IconName; size?: number } & Omit<SVGProps<SVGSVGElement>, "name">) {
  return (
    <svg
      width={size}
      height={size}
      viewBox="0 0 20 20"
      fill="none"
      stroke="currentColor"
      strokeWidth={1.6}
      strokeLinecap="round"
      strokeLinejoin="round"
      aria-hidden="true"
      focusable="false"
      {...props}
    >
      {paths[name]}
    </svg>
  );
}

/** The Knotree mark: a K that grows a leaf, paper glyph on a pine rounded square. */
export function KnotreeMark({ size = 26 }: { size?: number }) {
  return (
    <svg width={size} height={size} viewBox="0 0 64 64" aria-hidden="true" focusable="false">
      <rect width="64" height="64" rx="15" fill="#1f3d32" />
      <g stroke="#f6f5f2" strokeWidth="6.5" strokeLinecap="round" fill="none">
        <path d="M20 13v38M26 34l14 16" />
      </g>
      <path d="M23 34C21.85 21.9 32.8 12.06 45 10C41.9 20.41 33.05 32.17 23 34Z" fill="#f6f5f2" />
    </svg>
  );
}
