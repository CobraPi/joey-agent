# Byte-Preservation Edge Cases

This file is crafted so that any whitespace normalization, trailing-newline
stripping, or EOL conversion would break the byte round-trip.

## No trailing newline at end of file

The line above this has content. This is the last line — note the file ends here WITHOUT a newline: END