# nerka
Log analysis solution

## Assumptions
Tool shall optimize and accelerate work of analysing logs
to achieve this following aspects are considered
### compatibility with multiple formats:
tooling should allow easy integration with different inputs. e.g. plain text files, pcap files, dlt or other
### lightweight and feature rich
tooling cannot take a form of heavy monolitic application
e.g. someone could use it as file viewer only, it should be easy to deploy collection / analysis deployements as lightweigth aws nodes
### strong basis
The tool at least should surplus the functionity of the klogg
### multi platform
Bash, window, linux UI or macos - all should be supported to some extent
### #multiple deplyoments options??

### collaboration
allow for concurent work with the tooling, (analysis of the same logs by multiple people, sharing notes, markers, etc.)


First goal - have the functionality of klogg running on windows (Efficient regexp based)
