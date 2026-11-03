# Export dimensions

Choose **File > Export** and select a destination file. In **Resolution**, choose
a percentage of the timeline's dimensions or **Custom** to enter an independent
width and height. Returning to Custom restores the last dimensions you entered.

Width and height must be even and at least 2 pixels. The maximum size depends on
the GPU used for composition. Before creating the output file, export checks
the output dimensions against that GPU's texture limit. When a custom output has
a different aspect ratio, the timeline-sized intermediate must also fit within
the same limit. There is no fixed 8192-pixel or 32-megapixel cap.

For a custom size with a different aspect ratio, choose:

- **Fit with black bars** to keep the complete image and its proportions.
- **Fill and crop** to cover the output while preserving proportions.
- **Stretch to fill** to fill the output by changing the image's proportions.

These options resize the completed timeline image, including titles, overlays,
masks and clip transforms. High-precision composition stays in its work format
until the final conversion to the encoded image. Percentage presets preserve
the timeline's composition and do not use the custom aspect option.

Export dimensions do not change the project or timeline settings. The frame
rate remains the timeline frame rate; the output is an MP4 file with the video
and audio encoders selected in the export dialog.
