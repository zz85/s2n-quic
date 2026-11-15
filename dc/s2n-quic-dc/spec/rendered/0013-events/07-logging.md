## Diagnostic logging {#logging}

The endpoint writes log lines of its own at five levels: error, warning, info, debug and trace. Error and warning lines are present in every build. Info, debug and trace lines are present only in a build that asks for them, such as a test build. The integration that writes events as log lines is separate: it writes each event at the debug level wherever an application installs it.

### DC-0013-045 {#DC-0013-045}

The system MUST write error and warning log lines in every build.

### DC-0013-046 {#DC-0013-046}

Info, debug and trace log lines that the endpoint writes directly MUST NOT be present in a production build that doesn't ask for them.

### DC-0013-047 {#DC-0013-047}

Writing or leaving out a log line MUST NOT change the endpoint's protocol behavior.
