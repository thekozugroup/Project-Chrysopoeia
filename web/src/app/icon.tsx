import { ImageResponse } from "next/og";

export const size = { width: 32, height: 32 };
export const contentType = "image/png";

export default function Icon() {
  return new ImageResponse(
    (
      <div
        style={{
          width: 32,
          height: 32,
          display: "flex",
          alignItems: "center",
          justifyContent: "center",
          borderRadius: "50%",
          background: "#c9a227",
          color: "#0d0d0d",
          fontSize: 20,
          fontWeight: 700,
          fontFamily: "serif",
          lineHeight: 1,
        }}
      >
        C
      </div>
    ),
    { ...size }
  );
}
