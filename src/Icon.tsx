// Line icons drawn on one 24×24 grid so every toolbar / row button renders at the
// same size and weight (text glyphs and emoji such as ✎ and 🔑 do not).
const paths = {
  plus: "M12 5v14M5 12h14",
  edit: "M4 20h4L19 9l-4-4L4 16v4zM14 6l4 4",
  close: "M6 6l12 12M18 6L6 18",
  key: "M5 15a4 4 0 1 0 8 0a4 4 0 1 0-8 0M11.8 12.2 20 4M17 7l2 2M14.5 9.5l2 2",
  chevronRight: "M9 6l6 6-6 6",
  chevronDown: "M6 9l6 6 6-6",
  back: "M19 12H5M11 6l-6 6 6 6",
  forward: "M5 12h14M13 6l6 6-6 6",
  reload: "M20 12a8 8 0 1 1-2.3-5.7M20 4v5h-5",
  home: "M4 11l8-7 8 7M6 9.5V20h12V9.5",
  sidebar: "M4 5h16v14H4zM9 5v14",
  moon: "M20 14.5A8 8 0 0 1 9.5 4a8 8 0 1 0 10.5 10.5z",
  eye: "M2 12s3.5-7 10-7 10 7 10 7-3.5 7-10 7S2 12 2 12zM12 9a3 3 0 1 0 0 6a3 3 0 1 0 0-6",
  download: "M12 4v11M7 10l5 5 5-5M5 20h14",
  search: "M10.5 4a6.5 6.5 0 1 0 0 13a6.5 6.5 0 1 0 0-13M15.5 15.5 20 20",
  sun: "M12 8a4 4 0 1 0 0 8a4 4 0 1 0 0-8M12 2v2M12 20v2M4.9 4.9l1.4 1.4M17.7 17.7l1.4 1.4M2 12h2M20 12h2M4.9 19.1l1.4-1.4M17.7 6.3l1.4-1.4",
};

export type IconName = keyof typeof paths;

export function Icon({ name, size = 16 }: { name: IconName; size?: number }) {
  return (
    <svg
      width={size}
      height={size}
      viewBox="0 0 24 24"
      fill="none"
      stroke="currentColor"
      strokeWidth={2}
      strokeLinecap="round"
      strokeLinejoin="round"
      aria-hidden="true"
    >
      <path d={paths[name]} />
    </svg>
  );
}
