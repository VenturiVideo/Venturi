# Thumbnail regression fixture

`issue-61-original.jpg` preserves the reported input from
<https://github.com/VenturiVideo/Venturi/issues/61>. Its SHA-256 is
`9aefbb523c4c87f0f0699b98766d53590f8670e39a85201c50b4bb238e0622e9`.
The original attachment is
<https://github.com/user-attachments/assets/e0455271-f46e-44ce-b154-4786d6cc8f41>.

Generated JPEGs did not reproduce the reported seek failure. Keeping the
original bytes makes that regression part of the default offline test suite.
